// SPDX-License-Identifier: MIT OR Apache-2.0
//! `nowayos-isolation-probe`: a protection domain whose only job is to
//! attempt one memory access it was not granted, in NoWayOS's isolation
//! tests (`systems/isolation-test.system`, `systems/ui-isolation.system`,
//! `systems/input-isolation.system`, `systems/net-isolation.system`).
//!
//! The same program image runs as every probe PD, each a child of
//! `nowayos-isolation-parent`, and picks its probe by the PD name the system
//! description gives it. In `isolation-test`:
//!
//! * `probe_unmapped` reads `0x4000_0000` — the address of the bystander PD's
//!   private page in the *bystander's* address space. Nothing is mapped there
//!   in the probe's own address space, so the read must fault (a translation
//!   fault).
//! * `probe_readonly` reads its own page mapped `perms="r"` (a positive
//!   control: reading what was granted works), then writes to it. The write
//!   must fault (a permission fault): the same read-only rule protects the
//!   IPC demonstration's message region in the consumer PD.
//!
//! In `ui-isolation`, each probe maps exactly what the display PD or
//! the UI PD maps in `systems/ui-demo.system`, with the same role: a test
//! build of that PD's memory grants, with a probe in place of its code.
//!
//! * `display_writes_framebuffer` (the display's maps) reads the framebuffer
//!   it maps read-only, then writes to it: a permission fault.
//! * `ui_writes_display_index` (the UI's maps) reads the page holding the
//!   display's index on the `Present` ring (`present_control`), which the UI
//!   maps read-only, then writes to it: a permission fault.
//! * `ui_reads_display_private` (the UI's maps) writes a pixel into its own
//!   framebuffer and reads it back (a positive control), then reads
//!   `0x6000_0000`, where `display_side`, standing in for the display's
//!   private state, maps its private page: a translation fault.
//! * `ui_reads_device` (the UI's maps) reads `0x0900_0000`, the physical
//!   address of QEMU's PL011 UART, as a virtual address: a translation fault.
//!   It would catch only a device mapped at that very address; that the UI
//!   holds no device memory is checked by the grants check
//!   (`tools/check-system` rules 1, 6 and 11).
//!
//! In `input-isolation`, each probe maps exactly what the UI's
//! stand-in, `ui`, maps in `systems/input-broker-test.system`: the UI's
//! input ring from the input broker.
//!
//! * `ui_writes_driver_ring` writes `0x4100_0000`, where the broker maps a
//!   driver's ring and `broker_side` keeps its private page: a translation
//!   fault - the UI cannot reach a driver's ring to forge input.
//! * `ui_writes_broker_ring` reads the messages of its ring from the broker,
//!   which it maps read-only, then writes to them: a permission fault - the
//!   UI cannot alter the events the broker sends it.
//!
//! In the supervisor's tests, the same image plays three children of
//! the supervisor PD (`systems/supervisor-test.system`,
//! `systems/supervisor-panic.system`):
//!
//! * `faulter` checks its marker page - the marker must be absent, since
//!   every boot's memory is fresh - writes the marker, and waits; when
//!   `trigger` notifies it, it writes its read-only page: a permission fault.
//! * `trigger` notifies `faulter` once, standing in for hostile input.
//! * `panicker` panics, as a PD does on input it cannot handle.
//!
//! In the supervisor's counting tests, `faulter` and `trigger` play
//! the same parts (`systems/supervisor-counts.system`), and in the
//! fail-safe tests (`systems/failsafe-*.system`):
//!
//! * `kept_child`, which the supervisor keeps running in the fail-safe
//!   state, prints its start line, then writes its read-only page: a
//!   permission fault, which the supervisor reports and restarts nothing
//!   for;
//! * `stopped_child` prints its start line - which it must never do, as the
//!   supervisor stops it before it runs;
//! * `unnamed_child`, a child the supervisor's program does not name, does
//!   the same, and must never get to it either: the supervisor stops every
//!   child it does not keep, named or not. In
//!   `systems/supervisor-empty-list.system`, whose supervisor's program
//!   names no children at all, both are stopped;
//! * `kept_app`, which the supervisor's program in
//!   `systems/supervisor-bad-list.system` marks to keep although its class,
//!   `app`, is stopped in the fail-safe state, does the same: the
//!   supervisor reports the mark and stops it with the rest;
//! * `unnamed_faulter`, a child the supervisor's program does not name in
//!   `systems/supervisor-failsafe-release.system`, reads `0x4000_0000`,
//!   where nothing is mapped in it: a translation fault, which the
//!   supervisor would report through the console PD even in the release
//!   configuration - so the test sees, in both, that it never runs.
//!
//! In `net-isolation`, each probe maps exactly what the virtio-net
//! driver's client, `net_client`, maps in `systems/net-test.system`, with
//! its role (`network`): the four queues with the driver, the receive
//! buffers read-only and the transmit buffers.
//!
//! * `net_writes_driver_dma` writes `0x300_0000`, where the driver maps its
//!   receive queue's DMA memory and `driver_side` keeps its private page: a
//!   translation fault - the client cannot reach the driver's memory, nor
//!   through it the card's rings.
//! * `net_writes_rx_buffers` reads the receive buffers, which it maps
//!   read-only, then writes to them: a permission fault - the client cannot
//!   change a frame the card wrote.
//!
//! Either way seL4 stops the probe before its next instruction and reports
//! the fault to the parent. If the access ever succeeded, the probe prints
//! an `ISOLATION FAILURE` line, which the systems' expect files forbid —
//! except `systems/ui-isolation-widened.expect`, which requires one, to show
//! that the test can fail.
//!
//! A test instrument: it never belongs in a product image. The forbidden
//! accesses themselves need `unsafe`, confined to the `forbidden` module.

#![no_std]
#![no_main]
#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

#[allow(
    unsafe_code,
    reason = "the approved-unsafe module (tools/unsafe-allowlist.toml)"
)]
mod forbidden;

use forbidden::Unmapped;
use nowayos_region::{read_only, read_write};
use sel4_microkit::{
    Channel, ChannelSet, Handler, Infallible, debug_println, pd_name, protection_domain,
};

/// Start of every line this PD prints.
const TAG: &str = "NOWAYOS: isolation-probe:";

/// What the forbidden write puts in a read-only region.
const FORBIDDEN_VALUE: u32 = 0xBAD0_0001;

/// The pixel `ui_reads_display_private` draws, as an `Xrgb8888` word.
const WHITE: u32 = 0x00FF_FFFF;

/// What `faulter` writes in its marker page; a fresh boot must not find it.
const MARKER: u32 = 0x5EED_B007;

/// The channel between `trigger` and `faulter`.
const TRIGGER: Channel = Channel::new(0);

#[protection_domain]
fn init() -> ProbePd {
    match pd_name() {
        Ok(name @ "faulter") => return faulter_start(name),
        Ok("trigger") => {
            debug_println!("{TAG} trigger notifying faulter");
            TRIGGER.notify();
        }
        Ok("panicker") => panicker(),
        Ok(name @ "kept_child") => {
            debug_println!("{TAG} {name} starting");
            write_read_only(name, 0, "page");
        }
        Ok(name @ ("stopped_child" | "unnamed_child" | "kept_app")) => {
            debug_println!("{TAG} {name} starting");
        }
        Ok(name @ "unnamed_faulter") => read_unmapped(name, Unmapped::BystanderPage, ""),
        Ok(name @ "probe_unmapped") => read_unmapped(name, Unmapped::BystanderPage, ""),
        Ok(name @ "probe_readonly") => write_read_only(name, 0, "page"),
        Ok(name @ "display_writes_framebuffer") => write_read_only(name, 2, "framebuffer"),
        Ok(name @ "ui_writes_display_index") => {
            write_read_only(name, 0, "view of the display's index on the Present ring");
        }
        Ok(name @ "ui_reads_display_private") => {
            if draw_a_pixel(name) {
                read_unmapped(
                    name,
                    Unmapped::DisplayPrivatePage,
                    " (where display_side keeps its private page)",
                );
            }
        }
        Ok(name @ "ui_reads_device") => read_unmapped(
            name,
            Unmapped::UartRegisters,
            " (the physical address of QEMU's PL011 UART)",
        ),
        Ok(name @ "ui_writes_driver_ring") => write_unmapped(
            name,
            Unmapped::DriverRing,
            " (where the input broker maps a driver's ring and broker_side keeps its private page)",
        ),
        Ok(name @ "ui_writes_broker_ring") => {
            write_read_only(name, 0, "ring from the input broker");
        }
        Ok(name @ "net_writes_driver_dma") => write_unmapped(
            name,
            Unmapped::DriverDma,
            " (where the virtio-net driver maps its receive queue's DMA memory and driver_side keeps its private page)",
        ),
        Ok(name @ "net_writes_rx_buffers") => write_read_only(name, 0, "receive buffers"),
        Ok(other) => debug_println!("{TAG} {other}: no probe is defined for this PD name"),
        Err(_) => debug_println!("{TAG} this PD's name is not valid UTF-8"),
    }
    ProbePd { faulter: false }
}

/// Reads `target`, where nothing is mapped in this PD; `what` says what is
/// there elsewhere (`""` for nothing to add).
fn read_unmapped(name: &str, target: Unmapped, what: &str) {
    let address = target.address();
    debug_println!("{TAG} {name} reading {address:#x}{what}, which is not mapped in this PD");
    let value = forbidden::read_unmapped(target);
    debug_println!("{TAG} ISOLATION FAILURE: {name} read {value:#010x} from {address:#x}");
}

/// Writes `target`, where nothing is mapped in this PD; `what` says what is
/// there elsewhere.
fn write_unmapped(name: &str, target: Unmapped, what: &str) {
    let address = target.address();
    debug_println!("{TAG} {name} writing {address:#x}{what}, which is not mapped in this PD");
    forbidden::write_unmapped(target, FORBIDDEN_VALUE);
    debug_println!("{TAG} ISOLATION FAILURE: {name} wrote to {address:#x}");
}

/// Reads, then writes, this PD's read-only region `nowayos_ro_region_N`
/// (`index` N), which holds its `what`.
fn write_read_only(name: &str, index: usize, what: &str) {
    let region = match read_only(index) {
        Ok(region) => region,
        Err(err) => {
            debug_println!("{TAG} {name} cannot bind its read-only {what}: {err}");
            return;
        }
    };
    let vaddr = region.vaddr();
    let words = match region.into_words() {
        Ok(words) => words,
        Err(err) => {
            debug_println!("{TAG} {name} cannot view its read-only {what}: {err}");
            return;
        }
    };
    // Positive control: reading what was granted works.
    let value = words.load(0).unwrap_or(0);
    debug_println!("{TAG} {name} read {value:#010x} from its read-only {what} at {vaddr:#x}");
    debug_println!("{TAG} {name} writing to its read-only {what} at {vaddr:#x}");
    forbidden::write_read_only(vaddr, FORBIDDEN_VALUE);
    debug_println!("{TAG} ISOLATION FAILURE: {name} wrote to its read-only {what} at {vaddr:#x}");
}

/// Positive control for a UI probe: writes a pixel into this PD's
/// framebuffer (`nowayos_rw_region_2`, as the UI maps it) and reads it back.
/// Returns whether that worked.
fn draw_a_pixel(name: &str) -> bool {
    let region = match read_write(2) {
        Ok(region) => region,
        Err(err) => {
            debug_println!("{TAG} {name} cannot bind its framebuffer: {err}");
            return false;
        }
    };
    let vaddr = region.vaddr();
    let words = match region.into_words() {
        Ok(words) => words,
        Err(err) => {
            debug_println!("{TAG} {name} cannot view its framebuffer: {err}");
            return false;
        }
    };
    if !words.store(0, WHITE) || words.load(0) != Some(WHITE) {
        debug_println!("{TAG} {name} cannot draw into its framebuffer at {vaddr:#x}");
        return false;
    }
    debug_println!(
        "{TAG} {name} wrote {WHITE:#010x} to its framebuffer at {vaddr:#x} and read it back"
    );
    true
}

/// `faulter`'s start: checks its marker page, which every boot gives it
/// fresh, then writes the marker and waits for the trigger.
fn faulter_start(name: &'static str) -> ProbePd {
    let words = match read_write(0).and_then(|region| region.into_words()) {
        Ok(words) => words,
        Err(err) => {
            debug_println!("{TAG} {name} cannot bind its marker page: {err}");
            return ProbePd { faulter: false };
        }
    };
    if words.load(0) == Some(MARKER) {
        debug_println!("{TAG} {name}: the marker SURVIVED the reset");
    } else {
        debug_println!("{TAG} {name}: no marker - this boot's memory is fresh");
    }
    if words.store(0, MARKER) {
        debug_println!("{TAG} {name} wrote its marker; waiting for the trigger");
    }
    ProbePd { faulter: true }
}

/// `panicker`: panics, as a PD does on input it cannot handle.
#[allow(
    clippy::panic,
    reason = "a test instrument: the supervisor's test needs a PD that panics"
)]
fn panicker() {
    debug_println!("{TAG} panicker panicking");
    panic!("a deliberate panic, for the supervisor's test");
}

/// A probe has nothing to do after `init`: it has either been stopped by
/// seL4, or reported a failure - except `faulter`, which faults when
/// triggered.
struct ProbePd {
    faulter: bool,
}

impl Handler for ProbePd {
    type Error = Infallible;

    fn notified(&mut self, channels: ChannelSet) -> Result<(), Self::Error> {
        if self.faulter && channels.contains(TRIGGER) {
            debug_println!("{TAG} faulter triggered");
            write_read_only("faulter", 0, "page");
        }
        Ok(())
    }
}
