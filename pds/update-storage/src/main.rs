// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-update-storage`: a test instrument, the storage PD's
//! stand-in in `systems/update-test.system` and `update-trial-r*.system`.
//!
//! It keeps two copies of the update state, in its own memory, and two boot
//! slots, each a region of its own, as long as the region. It answers the
//! update PD's protected calls - read or write a copy of the state, write
//! image bytes to a slot, say how the device booted - through the page they
//! share (`nowayos_update_pd::storage`), and refuses any request out of
//! bounds. A test instrument: never in a demo or product system; a real
//! storage PD keeps them on the device's storage.
//!
//! **What it starts with.** In `update-test`, copy A is the test's
//! provisioned state (`test-update-state.bin`, which `tools/update-bundles`
//! writes), copy B empty, and slot A booted. In `update-trial` the
//! QEMU harness, playing the bootloader, hands it the device's storage as
//! the last run left it - both copies of the state, and the slot the
//! harness booted: QEMU's fw_cfg file `opt/nowayos/disk`, which the bundle
//! server, fw_cfg's one holder, copies into this PD's read-only disk region
//! before this PD starts, as it runs above it. It prints what the disk
//! holds, so the run's expect file checks the state it booted from. The
//! slots' bytes are not on that disk: the harness keeps each slot as the
//! image file whose SHA-256 this PD reported.
//!
//! **What it reports.** Each copy of the state written is decoded and
//! printed, for the expect file, and its bytes in hex, a line of 64 bytes
//! at a time, with the SHA-256 of what each slot was written this run - so
//! that the harness can take the state and the slots from this run's
//! serial log into the next run.
//!
//! The disk file: the magic `BDSK`, a 32-bit little-endian version (1), the
//! slot booted the same way (1 A, 2 B), four zero bytes, then copy A and
//! copy B, `COPY_LEN` bytes each (`nowayos_update::state`); a copy of all
//! zeros is empty.
//!
//! Channel 0, the update PD's calls. Regions: `nowayos_rw_region_0`, the
//! shared page; `nowayos_rw_region_1` and `_2`, slots A and B;
//! `nowayos_ro_region_0`, the provisioned state, or `nowayos_ro_region_1`,
//! the disk.

#![no_std]
#![no_main]

use nowayos_display::FrameMemory;
use nowayos_region::{Frame, ReadOnly, ReadWrite, Words, read_only, read_write};
use nowayos_signature::Sha256;
use nowayos_update::state::{COPY_LEN, Slot, StateCopy, decode_copy};
use nowayos_update_pd::storage::{BootFacts, OK, REFUSED, Request, WINDOW_LEN};
use nowayos_update_pd::{load, store};
use sel4_microkit::{
    Channel, Handler, Infallible, MessageInfo, debug_println, protection_domain, with_msg_regs,
    with_msg_regs_mut,
};

/// The disk's magic and its layout version.
const DISK_MAGIC: [u8; 4] = *b"BDSK";
const DISK_VERSION: u32 = 1;
/// The disk file's header: magic, version, the slot booted, four zeros.
const DISK_HEADER_LEN: usize = 16;
/// The disk file's whole length.
const DISK_LEN: usize = DISK_HEADER_LEN + 2 * COPY_LEN;

/// The bytes of a copy printed on one line.
const HEX_LINE: usize = 64;

#[protection_domain]
fn init() -> StoragePd {
    debug_println!("NOWAYOS: update-storage: starting");
    match start() {
        Ok(pd) => pd,
        Err(error) => {
            debug_println!("NOWAYOS: update-storage: gave up: {error}");
            StoragePd { state: None }
        }
    }
}

fn start() -> Result<StoragePd, &'static str> {
    let window = read_write(0)
        .and_then(|region| region.into_words())
        .map_err(|_| "the shared page")?;
    let slots = [slot(1, "slot A")?, slot(2, "slot B")?];
    let (booted, copies) = if let Ok(disk_region) = read_only(1) {
        let words = disk_region.into_words().map_err(|_| "the disk")?;
        disk(&words)?
    } else {
        let provisioned = read_only(0)
            .and_then(|region| region.into_words())
            .map_err(|_| "the provisioned state")?;
        let mut copy_a = [0u8; COPY_LEN];
        if !load(&provisioned, &mut copy_a) {
            return Err("the provisioned state");
        }
        describe("provisioned copy A", &copy_a);
        (Slot::A, [copy_a, [0; COPY_LEN]])
    };
    Ok(StoragePd {
        state: Some(State {
            window,
            slots,
            copies,
            booted,
        }),
    })
}

/// Slot region `index` (read-write region `index`), as long as the region.
fn slot(index: usize, name: &'static str) -> Result<SlotRegion, &'static str> {
    let bytes = read_write(index)
        .and_then(|region| region.into_frame(0))
        .map_err(|_| name)?;
    Ok(SlotRegion { bytes, written: 0 })
}

/// Reads the disk the harness handed over, copied out of its region before
/// it is checked, and prints what it holds.
fn disk(words: &Words<ReadOnly>) -> Result<(Slot, [[u8; COPY_LEN]; 2]), &'static str> {
    let mut bytes = [0u8; DISK_LEN];
    if !load(words, &mut bytes) {
        return Err("the disk region is smaller than a disk");
    }
    let word = |at: usize| {
        bytes
            .get(at..at.saturating_add(4))
            .and_then(|word| <[u8; 4]>::try_from(word).ok())
            .map(u32::from_le_bytes)
    };
    if bytes.get(..4) != Some(&DISK_MAGIC[..])
        || word(4) != Some(DISK_VERSION)
        || word(12) != Some(0)
    {
        return Err("no version 1 disk: did the harness give QEMU opt/nowayos/disk?");
    }
    let booted = match word(8) {
        Some(1) => Slot::A,
        Some(2) => Slot::B,
        _ => return Err("the disk names no slot booted"),
    };
    let mut copies = [[0u8; COPY_LEN]; 2];
    for (index, copy) in copies.iter_mut().enumerate() {
        let at = DISK_HEADER_LEN.saturating_add(index.saturating_mul(COPY_LEN));
        let from = bytes
            .get(at..at.saturating_add(COPY_LEN))
            .ok_or("the disk")?;
        copy.copy_from_slice(from);
    }
    debug_println!(
        "NOWAYOS: update-storage: the disk the last run left: booted slot {}",
        slot_name(booted)
    );
    describe("copy A on the disk", &copies[0]);
    describe("copy B on the disk", &copies[1]);
    Ok((booted, copies))
}

/// The PD; `None` only if it could not start.
struct StoragePd {
    state: Option<State>,
}

/// A slot: its region, as bytes, and how far into it this run wrote.
struct SlotRegion {
    bytes: Frame<ReadWrite>,
    written: usize,
}

struct State {
    window: Words<ReadWrite>,
    slots: [SlotRegion; 2],
    copies: [[u8; COPY_LEN]; 2],
    booted: Slot,
}

impl State {
    /// Does `request`; the reply's message registers if it was in bounds
    /// and done.
    fn serve(&mut self, request: Request) -> Option<[u64; 2]> {
        match request {
            Request::ReadState(copy) => self
                .copies
                .get(index_of_copy(copy))
                .is_some_and(|bytes| store(&self.window, bytes))
                .then_some([0; 2]),
            Request::WriteState(copy) => {
                let mut bytes = [0u8; COPY_LEN];
                if !load(&self.window, &mut bytes) {
                    return None;
                }
                *self.copies.get_mut(index_of_copy(copy))? = bytes;
                let name = copy_name(copy);
                describe(name, &bytes);
                report(name, &bytes, &self.slots);
                Some([0; 2])
            }
            Request::WriteSlot { slot, offset, len } => {
                self.write_slot(slot, offset, len).then_some([0; 2])
            }
            Request::Boot => {
                let slot_len = self.slots.first().map(|slot| slot.bytes.len())?;
                let facts = BootFacts {
                    slot: self.booted,
                    slot_len: u64::try_from(slot_len).ok()?,
                };
                Some(facts.words())
            }
        }
    }

    /// Writes the page's first `len` bytes at `offset` in `slot`, if they
    /// fit both.
    fn write_slot(&mut self, slot: Slot, offset: u64, len: u32) -> bool {
        let (Ok(offset), Ok(len)) = (usize::try_from(offset), usize::try_from(len)) else {
            return false;
        };
        let Some(region) = self.slots.get_mut(index_of_slot(slot)) else {
            return false;
        };
        let Some(end) = offset.checked_add(len).filter(|_| len <= WINDOW_LEN) else {
            return false;
        };
        let mut bytes = [0u8; WINDOW_LEN];
        let Some(bytes) = bytes.get_mut(..len) else {
            return false;
        };
        // Copied out of the shared page, then stored: false past the slot's end.
        if !load(&self.window, bytes) || !region.bytes.store(offset, bytes) {
            return false;
        }
        region.written = region.written.max(end);
        true
    }
}

impl Handler for StoragePd {
    type Error = Infallible;

    fn protected(
        &mut self,
        _channel: Channel,
        msg_info: MessageInfo,
    ) -> Result<MessageInfo, Self::Error> {
        let words = with_msg_regs(|regs| {
            let mut words = [0u64; 3];
            for (word, reg) in words.iter_mut().zip(regs.iter().take(msg_info.count())) {
                *word = *reg;
            }
            words
        });
        let reply = match (
            self.state.as_mut(),
            Request::received(msg_info.label(), words),
        ) {
            (Some(state), Some(request)) => state.serve(request),
            _ => None,
        };
        Ok(match reply {
            Some(words) => {
                with_msg_regs_mut(|regs| {
                    for (reg, word) in regs.iter_mut().zip(words) {
                        *reg = word;
                    }
                });
                MessageInfo::new(OK, words.len())
            }
            None => MessageInfo::new(REFUSED, 0),
        })
    }
}

const fn index_of_copy(copy: StateCopy) -> usize {
    match copy {
        StateCopy::A => 0,
        StateCopy::B => 1,
    }
}

const fn index_of_slot(slot: Slot) -> usize {
    match slot {
        Slot::A => 0,
        Slot::B => 1,
    }
}

const fn slot_name(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "A",
        Slot::B => "B",
    }
}

const fn copy_name(copy: StateCopy) -> &'static str {
    match copy {
        StateCopy::A => "copy A written",
        StateCopy::B => "copy B written",
    }
}

/// Prints what a copy of the state holds, for the expect file.
fn describe(what: &str, bytes: &[u8; COPY_LEN]) {
    if bytes.iter().all(|byte| *byte == 0) {
        debug_println!("NOWAYOS: update-storage: {what}: empty");
        return;
    }
    match decode_copy(bytes) {
        Ok((generation, state)) => {
            let boots = slot_name(state.boots);
            match state.trial {
                Some(trial) => debug_println!(
                    "NOWAYOS: update-storage: {what}: generation {generation}: slot {boots} boots; slot {} marked for one trial, security version {}; rollback index {}; trust list v{}",
                    slot_name(trial.slot),
                    trial.security_version,
                    state.rollback_index,
                    state.trust_list_version()
                ),
                None => debug_println!(
                    "NOWAYOS: update-storage: {what}: generation {generation}: slot {boots} boots; nothing marked; rollback index {}; trust list v{}",
                    state.rollback_index,
                    state.trust_list_version()
                ),
            }
        }
        Err(error) => debug_println!("NOWAYOS: update-storage: {what}: does not decode: {error}"),
    }
}

/// Prints a copy written as hex, a line at a time, and the SHA-256 of what
/// each slot was written this run: what the harness carries into the next
/// run.
fn report(what: &str, bytes: &[u8; COPY_LEN], slots: &[SlotRegion; 2]) {
    let lines = COPY_LEN.div_ceil(HEX_LINE);
    for (line, chunk) in bytes.chunks(HEX_LINE).enumerate() {
        let mut text = [0u8; 2 * HEX_LINE];
        let len = hex(chunk, &mut text);
        let text = core::str::from_utf8(text.get(..len).unwrap_or_default()).unwrap_or_default();
        debug_println!(
            "NOWAYOS: update-storage: {what}: bytes {}/{lines}: {text}",
            line.saturating_add(1)
        );
    }
    for (slot, region) in [Slot::A, Slot::B].into_iter().zip(slots) {
        if region.written == 0 {
            continue;
        }
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 512];
        let mut at = 0;
        while at < region.written {
            let take = region.written.saturating_sub(at).min(buffer.len());
            let Some(piece) = buffer.get_mut(..take) else {
                return;
            };
            if !region.bytes.load(at, piece) {
                return;
            }
            digest.update(piece);
            at = at.saturating_add(take);
        }
        let digest = digest.finish();
        let mut text = [0u8; 64];
        let len = hex(&digest, &mut text);
        let text = core::str::from_utf8(text.get(..len).unwrap_or_default()).unwrap_or_default();
        debug_println!(
            "NOWAYOS: update-storage: slot {} holds {} bytes written this run, SHA-256 {text}",
            slot_name(slot),
            region.written
        );
    }
}

/// Writes `bytes` as lower-case hex into `out`; the length written.
fn hex(bytes: &[u8], out: &mut [u8]) -> usize {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut len = 0_usize;
    for (byte, pair) in bytes.iter().zip(out.chunks_exact_mut(2)) {
        if let [high, low] = pair {
            *high = DIGITS.get(usize::from(byte >> 4)).copied().unwrap_or(b'0');
            *low = DIGITS.get(usize::from(byte & 0xF)).copied().unwrap_or(b'0');
            len = len.saturating_add(2);
        }
    }
    len
}
