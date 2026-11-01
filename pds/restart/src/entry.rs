// SPDX-License-Identifier: MIT OR Apache-2.0
//! The restart entry, in a PD that can be restarted (PD builds only; see the
//! crate documentation).
//!
//! The PD's program starts at `nowayos_restart_entry` (its build.rs sets the
//! linker's `--entry`), cold or restarted. The entry calls [`prepare`] on
//! the stack Microkit gave the PD - at a restart, its parent sets the same
//! stack pointer - then branches to rust-sel4's `_start`, which sets up the
//! runtime and runs the PD's `init`. If [`prepare`] refuses, the entry
//! executes a permanently undefined instruction: the PD stops with a fault
//! its parent sees, and never runs on memory it could not check or restore.
//!
//! Written from the Arm A64 instruction set (Arm DDI 0487: `BL`, `CBNZ`, `B`,
//! `UDF`), the ELF specification's program headers (`PT_LOAD`, `PF_W`), and
//! rust-sel4 v5.0.0's runtime (`sel4-runtime-common`'s `_start`,
//! `sel4-phdrs`).

use core::arch::global_asm;
use core::fmt;
use core::slice;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use sel4_microkit::debug_println;
use sel4_phdrs::{PT_LOAD, locate_phdrs};

use crate::image::{Image, ImageError};
use crate::{RESTART, Start};

/// The program header flag of a writable segment (ELF's `PF_W`).
const PF_W: u32 = 2;

// The restart image's address and size, which the Microkit tool patches in
// from the PD's map of it (setvar_vaddr="nowayos_restart_image_vaddr",
// setvar_size="nowayos_restart_image_size"). In .data, as nowayos-region's
// symbols are, so that the program image has bytes for the tool to patch;
// they are kept ranges of the image, so a restart keeps their values.
// SAFETY (export_name, link_section): each name is used by no other symbol,
// and .data is where the Microkit tool expects a patched symbol's bytes.
#[unsafe(export_name = "nowayos_restart_image_vaddr")]
#[unsafe(link_section = ".data")]
static IMAGE_VADDR: AtomicUsize = AtomicUsize::new(0);

#[unsafe(export_name = "nowayos_restart_image_size")]
#[unsafe(link_section = ".data")]
static IMAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// How this start began, as a [`Start`]. In `.bss`, so a restart zeros it
/// with the rest before [`prepare`] sets it.
static STARTED: AtomicU8 = AtomicU8::new(0);

/// How this start of the PD began: a cold start, with or without its memory
/// checked against its restart image, or a restart.
#[must_use]
pub fn started() -> Start {
    Start::from_u8(STARTED.load(Ordering::Relaxed))
}

global_asm!(
    ".section .text.nowayos_restart_entry, \"ax\", %progbits",
    ".global nowayos_restart_entry",
    ".type nowayos_restart_entry, %function",
    ".balign 4",
    "nowayos_restart_entry:",
    // x0: 0 at a cold start, RESTART at a restart. prepare returns 0 to go on.
    "    bl {prepare}",
    "    cbnz x0, 1f",
    "    b _start",
    // Refused: stop with a fault the parent sees.
    "1:  udf #0",
    prepare = sym prepare,
);

/// Why the restart entry refused to go on.
enum Refusal {
    /// `x0` was neither 0 nor [`RESTART`].
    Mode(u64),
    /// A restart, but no restart image is mapped.
    NoImage,
    /// The runtime's view of the program headers is missing.
    NoProgramHeaders,
    /// A segment's address does not fit this address space.
    Address,
    /// The image, or the memory checked against it.
    Image(ImageError),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mode(mode) => {
                write!(f, "x0 is {mode:#x}: neither a cold start (0) nor a restart")
            }
            Self::NoImage => f.write_str("a restart, but no restart image is mapped"),
            Self::NoProgramHeaders => f.write_str("the program headers cannot be found"),
            Self::Address => f.write_str("a segment's address does not fit this address space"),
            Self::Image(error) => write!(f, "{error}"),
        }
    }
}

/// Runs before the runtime, from the restart entry, with its `x0`: checks
/// (cold start) or restores (restart) the PD's writable memory. Returns 0 to
/// go on to `_start`, anything else to stop the PD.
extern "C" fn prepare(mode: u64) -> u64 {
    let outcome = match mode {
        0 => load(true),
        RESTART => load(false),
        _ => Err(Refusal::Mode(mode)),
    };
    match outcome {
        Ok(start) => {
            STARTED.store(start as u8, Ordering::Relaxed);
            0
        }
        Err(refusal) => {
            debug_println!("NOWAYOS: restart: the restart entry refused: {refusal}");
            1
        }
    }
}

/// Checks (`cold`) or restores the PD's writable segments against its
/// restart image, if it has one.
fn load(cold: bool) -> Result<Start, Refusal> {
    let vaddr = IMAGE_VADDR.load(Ordering::Relaxed);
    let size = IMAGE_SIZE.load(Ordering::Relaxed);
    if vaddr == 0 {
        return if cold {
            Ok(Start::Cold)
        } else {
            Err(Refusal::NoImage)
        };
    }
    // SAFETY: the Microkit tool patched `vaddr` and `size` with the PD's map
    // of its restart image: read-only here for the PD's whole life, and
    // writable by no PD (tools/check-system, rules 12 and 13), so the bytes
    // stay valid and unchanged for 'static.
    let bytes: &'static [u8] = unsafe { slice::from_raw_parts(vaddr as *const u8, size) };
    let image = Image::parse(bytes).map_err(Refusal::Image)?;
    let headers = locate_phdrs().map_err(|_| Refusal::NoProgramHeaders)?;
    image
        .matches_program(
            headers
                .iter()
                .filter(|header| header.p_type == PT_LOAD && header.p_flags & PF_W != 0)
                .map(|header| {
                    (
                        header.p_vaddr as u64,
                        header.p_filesz as u64,
                        header.p_memsz as u64,
                    )
                }),
        )
        .map_err(Refusal::Image)?;
    for segment in image.segments() {
        let start = usize::try_from(segment.vaddr).map_err(|_| Refusal::Address)?;
        let len = usize::try_from(segment.memsz).map_err(|_| Refusal::Address)?;
        if cold {
            // SAFETY: `start..start + len` is one of this program's own
            // writable loadable segments - its program headers say so,
            // checked just above - mapped for the PD's whole life; nothing
            // writes it while this view lives, as the runtime has not
            // started.
            let memory = unsafe { slice::from_raw_parts(start as *const u8, len) };
            image.check(&segment, memory).map_err(Refusal::Image)?;
        } else {
            // SAFETY: as above; and nothing else reads or writes it while
            // this unique view lives: the runtime has not started again,
            // this function reaches the statics there only before and after
            // the view (IMAGE_VADDR and IMAGE_SIZE are kept, never written),
            // and the stack is Microkit's own region, outside every segment.
            let memory = unsafe { slice::from_raw_parts_mut(start as *mut u8, len) };
            image.restore(&segment, memory).map_err(Refusal::Image)?;
        }
    }
    Ok(if cold {
        Start::ColdChecked
    } else {
        Start::Restarted
    })
}
