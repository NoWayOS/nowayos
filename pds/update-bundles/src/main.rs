// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-update-bundles`: a test instrument, the bundle server in
//! `systems/update-test.system`, in the TLS PD's place - and in
//! `update-trial-r*.system` the health check's stand-in, and the
//! harness's hand inside the guest, too.
//!
//! It reads the run's bundles, if the run has any, from QEMU's fw_cfg file
//! `opt/nowayos/bundles` into its DMA region, in one transfer: made with
//! keys made for this run (`tools/update-bundles`, `tools/update-trial`)
//! and handed to QEMU at run time, so no build holds them. It then
//! offers the update PD one bundle at a time: each bundle's chunk records on
//! the ring, then a notification; the next when the update PD notifies back.
//! fw_cfg is test only: never in a demo or product system, and only
//! this PD maps it (grants-check rule 7).
//!
//! The bundles file: a 32-bit little-endian count of bundles, each bundle's
//! length the same way, then the bundles, each a whole number of chunk
//! records.
//!
//! **In update-trial** the QEMU harness, playing the bootloader,
//! hands over two more files. `opt/nowayos/disk`, the device's update state
//! as the last run left it, it copies into the region the storage stand-in
//! reads it from (read-write region 1), before the stand-in starts: it runs
//! above it. And `opt/nowayos/health`, a trial boot's health check: it
//! stands in for the supervisor's heartbeats and the product's own check,
//! and when the update PD asks for the verdict
//! (`nowayos_update_pd::HEALTH_ASK`) it answers `pass`, `fail` or `late` -
//! or, for `fault`, faults instead: a PD of the trial release failing, so
//! that the supervisor resets the board. Where the image has a release
//! marker (`test-release-N.bin`, read-only region 1), it says which release
//! it is first.
//!
//! Channel 0 to the update PD, both ways, which the update PD may call.
//! Device 0, fw_cfg's registers; DMA region 0, whose physical address it
//! learns, as long as the longest file. Regions: `nowayos_rw_region_0` and
//! `nowayos_ro_region_0`, the ring to the update PD (messages read-write,
//! control read-only); `nowayos_ro_region_1`, the release marker;
//! `nowayos_rw_region_1`, the disk for the storage stand-in.

#![no_std]
#![no_main]

use nowayos_hw::fw_cfg::{File, FwCfg, REQUEST_LEN};
use nowayos_hw::{DmaRegion, device, dma_region};
use nowayos_ipc::Producer;
use nowayos_region::{RingTx, read_only, read_write, ring_producer};
use nowayos_update::chunk::RECORD_LEN;
use nowayos_update_pd::{
    HEALTH_ASK, RING_SLOTS, VERDICT_FAILED, VERDICT_LATE, VERDICT_PASSED, store,
};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, MessageInfo, debug_println, protection_domain,
};

/// The update PD.
const UPDATE: Channel = Channel::new(0);

/// The most bundles the file may hold.
const MAX_BUNDLES: usize = 16;

/// Where a file's bytes start in the DMA region: after the request.
const DATA_AT: usize = REQUEST_LEN;

/// The longest health script: a word and a line end.
const SCRIPT_LEN: usize = 16;

/// The longest disk: what the storage stand-in's region holds.
const DISK_MAX: usize = 4096;

#[protection_domain]
fn init() -> BundlesPd {
    debug_println!("NOWAYOS: update-bundles: starting");
    release();
    let mut pd = BundlesPd {
        bundles: None,
        script: None,
    };
    if let Err(error) = start(&mut pd) {
        debug_println!("NOWAYOS: update-bundles: gave up: {error}");
        return BundlesPd {
            bundles: None,
            script: None,
        };
    }
    if let Some(bundles) = pd.bundles.as_mut() {
        debug_println!(
            "NOWAYOS: update-bundles: {} bundles from fw_cfg's opt/nowayos/bundles",
            bundles.count
        );
        bundles.offer();
    }
    pd
}

/// Says which release the image is, where it has a release marker.
fn release() {
    let release = read_only(1)
        .and_then(|region| region.into_words())
        .ok()
        .and_then(|words| words.load(0));
    if let Some(release) = release {
        debug_println!("NOWAYOS: update-bundles: this image is release {release}");
    }
}

/// What the health check's stand-in answers.
#[derive(Clone, Copy)]
enum Script {
    /// This verdict (the reply's label), and what it says.
    Verdict(u64, &'static str),
    /// Fault.
    Fault,
}

/// Reads what the harness handed over: the disk, the health script, the
/// bundles.
fn start(pd: &mut BundlesPd) -> Result<(), &'static str> {
    let mut fw_cfg = device(0)
        .and_then(FwCfg::new)
        .map_err(|_| "no fw_cfg device with DMA")?;
    let mut region = dma_region(0).map_err(|_| "the DMA region")?;
    if let Some(file) = find(&mut fw_cfg, &mut region, b"opt/nowayos/disk")? {
        disk(&mut fw_cfg, &mut region, file)?;
    }
    if let Some(file) = find(&mut fw_cfg, &mut region, b"opt/nowayos/health")? {
        pd.script = Some(script(&mut fw_cfg, &mut region, file)?);
    }
    if let Some(file) = find(&mut fw_cfg, &mut region, b"opt/nowayos/bundles")? {
        pd.bundles = Some(bundles(&mut fw_cfg, region, file)?);
    } else if pd.script.is_none() {
        debug_println!("NOWAYOS: update-bundles: no bundles and no health check this run");
    }
    Ok(())
}

/// The fw_cfg file called `name`, if the run has it.
fn find(
    fw_cfg: &mut FwCfg<'_>,
    region: &mut DmaRegion,
    name: &[u8],
) -> Result<Option<File>, &'static str> {
    fw_cfg
        .file(name, region, 0)
        .map_err(|_| "fw_cfg's file directory")
}

/// Copies the disk into the storage stand-in's region.
fn disk(fw_cfg: &mut FwCfg<'_>, region: &mut DmaRegion, file: File) -> Result<(), &'static str> {
    let len = usize::try_from(file.size)
        .ok()
        .filter(|len| *len <= DISK_MAX)
        .ok_or("opt/nowayos/disk is longer than the stand-in's region")?;
    fw_cfg
        .read_item(file.key, region, 0, file.size)
        .map_err(|_| "opt/nowayos/disk")?;
    let mut bytes = [0u8; DISK_MAX];
    let bytes = bytes.get_mut(..len).ok_or("opt/nowayos/disk")?;
    region
        .read(DATA_AT, bytes)
        .map_err(|_| "opt/nowayos/disk")?;
    let to = read_write(1)
        .and_then(|region| region.into_words())
        .map_err(|_| "the storage stand-in's disk region")?;
    if !store(&to, bytes) {
        return Err("the storage stand-in's disk region is too small");
    }
    debug_println!("NOWAYOS: update-bundles: the disk handed to the storage stand-in: {len} bytes");
    Ok(())
}

/// Reads the health script.
fn script(
    fw_cfg: &mut FwCfg<'_>,
    region: &mut DmaRegion,
    file: File,
) -> Result<Script, &'static str> {
    let len = usize::try_from(file.size)
        .ok()
        .filter(|len| *len <= SCRIPT_LEN)
        .ok_or("opt/nowayos/health is longer than a script")?;
    fw_cfg
        .read_item(file.key, region, 0, file.size)
        .map_err(|_| "opt/nowayos/health")?;
    let mut text = [0u8; SCRIPT_LEN];
    let text = text.get_mut(..len).ok_or("opt/nowayos/health")?;
    region
        .read(DATA_AT, text)
        .map_err(|_| "opt/nowayos/health")?;
    let script = match text.trim_ascii() {
        b"pass" => Script::Verdict(VERDICT_PASSED, "passed"),
        b"fail" => Script::Verdict(VERDICT_FAILED, "failed"),
        b"late" => Script::Verdict(VERDICT_LATE, "came after the deadline"),
        b"fault" => Script::Fault,
        _ => return Err("opt/nowayos/health is not pass, fail, late or fault"),
    };
    debug_println!("NOWAYOS: update-bundles: standing in for this trial boot's health check");
    Ok(script)
}

/// Reads the bundles, and binds the ring to the update PD.
fn bundles(
    fw_cfg: &mut FwCfg<'_>,
    mut region: DmaRegion,
    file: File,
) -> Result<Bundles, &'static str> {
    fw_cfg
        .read_item(file.key, &mut region, 0, file.size)
        .map_err(|_| "the bundles do not fit the DMA region")?;
    let size = usize::try_from(file.size).map_err(|_| "the bundles' size")?;
    let count = word(&region, 0).ok_or("the bundles' count")?;
    if count == 0 || count > MAX_BUNDLES {
        return Err("no bundle, or more than the server holds");
    }
    let mut bundles = [(0usize, 0usize); MAX_BUNDLES];
    let mut at = count.saturating_add(1).saturating_mul(4);
    for (index, bundle) in bundles.iter_mut().take(count).enumerate() {
        let len = word(&region, index.saturating_add(1)).ok_or("a bundle's length")?;
        if len == 0 || len % RECORD_LEN != 0 {
            return Err("a bundle that is not whole chunk records");
        }
        if len / RECORD_LEN > RING_SLOTS {
            return Err("a bundle longer than the ring");
        }
        *bundle = (at, len);
        at = at.saturating_add(len);
    }
    if at != size {
        return Err("the bundles' lengths do not add up to the file");
    }
    let ring = read_write(0)
        .and_then(|messages| ring_producer(messages, read_only(0)?))
        .map_err(|_| "the ring to the update PD")?;
    Ok(Bundles {
        region,
        ring,
        bundles,
        count,
        next: 0,
    })
}

/// A PD of the trial release fails: this one, on purpose, so that the
/// supervisor resets the board (a fault fails the trial).
#[allow(
    clippy::panic,
    reason = "a test instrument: the trial release's fault, which reaches the supervisor as one"
)]
fn fault_on_purpose() -> ! {
    debug_println!("NOWAYOS: update-bundles: the health check: faulting on purpose");
    panic!("update-bundles: a PD of the trial release faults");
}

/// The file's 32-bit little-endian word `index`.
fn word(region: &DmaRegion, index: usize) -> Option<usize> {
    let mut bytes = [0u8; 4];
    region
        .read(DATA_AT.checked_add(index.checked_mul(4)?)?, &mut bytes)
        .ok()?;
    usize::try_from(u32::from_le_bytes(bytes)).ok()
}

/// The PD: the bundles it offers, and the health check it stands in for.
struct BundlesPd {
    bundles: Option<Bundles>,
    script: Option<Script>,
}

struct Bundles {
    region: DmaRegion,
    ring: RingTx<RING_SLOTS, RECORD_LEN>,
    /// Each bundle's offset in the file and length.
    bundles: [(usize, usize); MAX_BUNDLES],
    count: usize,
    /// The bundle to offer next.
    next: usize,
}

impl Bundles {
    /// Puts the next bundle's records on the ring and tells the update PD;
    /// says so when every bundle has been offered.
    fn offer(&mut self) {
        let Some(&(at, len)) = self
            .bundles
            .get(self.next)
            .filter(|_| self.next < self.count)
        else {
            debug_println!(
                "NOWAYOS: update-bundles: all {} bundles offered",
                self.count
            );
            return;
        };
        self.next = self.next.saturating_add(1);
        let Ok(mut tx) = Producer::new(&self.ring) else {
            debug_println!("NOWAYOS: update-bundles: gave up: the ring is broken");
            return;
        };
        let mut record = [0u8; RECORD_LEN];
        for offset in (at..at.saturating_add(len)).step_by(RECORD_LEN) {
            let read = DATA_AT
                .checked_add(offset)
                .is_some_and(|from| self.region.read(from, &mut record).is_ok());
            if !read || tx.try_send_bytes(&record).is_err() {
                debug_println!(
                    "NOWAYOS: update-bundles: gave up: bundle {} could not be put on the ring",
                    self.next
                );
                return;
            }
        }
        debug_println!(
            "NOWAYOS: update-bundles: offering bundle {} of {}",
            self.next,
            self.count
        );
        UPDATE.notify();
    }
}

impl Handler for BundlesPd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if let Some(bundles) = self.bundles.as_mut()
            && channels.contains(UPDATE)
        {
            bundles.offer();
        }
        Ok(())
    }

    fn protected(
        &mut self,
        channel: Channel,
        msg_info: MessageInfo,
    ) -> Result<MessageInfo, Self::Error> {
        if channel != UPDATE || msg_info.label() != HEALTH_ASK {
            return Ok(MessageInfo::new(0, 0));
        }
        let verdict = match self.script {
            Some(Script::Verdict(label, said)) => {
                debug_println!("NOWAYOS: update-bundles: the health check {said}");
                label
            }
            Some(Script::Fault) => fault_on_purpose(),
            None => {
                debug_println!("NOWAYOS: update-bundles: no health check this run");
                0
            }
        };
        Ok(MessageInfo::new(verdict, 0))
    }
}
