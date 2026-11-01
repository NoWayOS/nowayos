// SPDX-License-Identifier: MIT OR Apache-2.0
//! The VideoCore mailbox: one property call to the firmware, *get
//! temperature*, the SoC's temperature in thousandths of a degree - for the
//! fan PD, which has no other thermometer.
//!
//! Raspberry Pi's firmware wiki gives the mailbox and the call: the ARM
//! reads mailbox 0 (`READ` at 0x00, `STATUS` at 0x18) and writes mailbox 1
//! (`WRITE` at 0x20, `STATUS` at 0x38); `STATUS` bit 31 full, bit 30 empty;
//! a message is a 16-byte-aligned buffer's address with the channel in its
//! low four bits, 8 for property tags, which take physical addresses; the
//! buffer holds its size, a request code (0; answered 0x8000_0000), tags
//! and an end tag; *get temperature* is tag 0x0003_0006, a 4-byte request
//! (the sensor, 0) answered with 8 bytes (the sensor and the temperature).
//! The mailbox's registers are at 0x10_7c01_3880 (the pinned device tree's
//! `mailbox@7c013880`, `brcm,bcm2835-mbox`), in a page that holds nothing
//! else the device tree names.
//!
//! The buffer is a DMA region mapped uncached, so the firmware and this PD
//! see the same bytes; the answer is copied out, then checked. Every wait is
//! bounded on the counter. Only this call is ever made: a message names its
//! buffer, and the buffer only ever holds this request.

use safe_mmio::UniqueMmioPointer;

use crate::{Device, DmaRegion, HwError, cpu};

/// The mailbox registers' offset in their page (0x10_7c01_3880).
const MAILBOX: usize = 0x880;
/// Mailbox 0's `READ`: a message from the firmware; reading pops it.
const READ: usize = MAILBOX;
/// Mailbox 0's `STATUS`.
const READ_STATUS: usize = MAILBOX + 0x18;
/// Mailbox 1's `WRITE`: a message to the firmware.
const WRITE: usize = MAILBOX + 0x20;
/// Mailbox 1's `STATUS`.
const WRITE_STATUS: usize = MAILBOX + 0x38;
/// `STATUS`'s full bit.
const FULL: u32 = 1 << 31;
/// `STATUS`'s empty bit.
const EMPTY: u32 = 1 << 30;
/// The property-tag channel, ARM to VideoCore.
const PROPERTY: u32 = 8;

/// *Get temperature*'s tag.
pub const GET_TEMPERATURE: u32 = 0x0003_0006;
/// A buffer's request code; and the answer to one processed.
const REQUEST: u32 = 0;
const ANSWERED: u32 = 0x8000_0000;
/// A tag's answer: bit 31, with its value's length in bits 30:0.
const TAG_ANSWERED: u32 = 0x8000_0000;
/// The message: 8 words, 32 bytes, a multiple of 16.
pub const MESSAGE_WORDS: usize = 8;
const MESSAGE_BYTES: usize = MESSAGE_WORDS * 4;
/// How long the firmware has to answer: 100 ms.
const ANSWER_WITHIN_MS: u64 = 100;
/// Stale messages drained before asking, at most.
const DRAIN_AT_MOST: u32 = 16;

/// *Get temperature* for sensor 0: the buffer the firmware is given.
#[must_use]
pub const fn temperature_request() -> [u32; MESSAGE_WORDS] {
    // Size, request code; the tag, its value's 8 bytes, its request code
    // (0), the sensor (0) and room for the temperature; the end tag.
    [
        MESSAGE_BYTES as u32,
        REQUEST,
        GET_TEMPERATURE,
        8,
        0,
        0,
        0,
        0,
    ]
}

/// The temperature in an answer to [`temperature_request`], in thousandths
/// of a degree; any other answer is refused.
///
/// # Errors
///
/// [`HwError::Mailbox`] if the buffer was not processed, or the tag not
/// answered with sensor 0's 8 bytes, or the temperature is out of range.
pub fn parse_temperature(answer: &[u32; MESSAGE_WORDS]) -> Result<i32, HwError> {
    let [size, code, tag, value_len, tag_code, sensor, value, end] = *answer;
    if size != MESSAGE_BYTES as u32 || code != ANSWERED {
        return Err(HwError::Mailbox {
            reason: "the firmware did not process the request",
        });
    }
    if tag != GET_TEMPERATURE
        || value_len != 8
        || tag_code != TAG_ANSWERED | 8
        || sensor != 0
        || end != 0
    {
        return Err(HwError::Mailbox {
            reason: "the firmware's answer is not a temperature",
        });
    }
    i32::try_from(value).map_err(|_| HwError::Mailbox {
        reason: "the firmware's temperature is out of range",
    })
}

/// The mailbox and the buffer it is given. Get one from [`mailbox`].
#[derive(Debug)]
pub struct Mailbox {
    regs: UniqueMmioPointer<'static, [u32]>,
    buffer: DmaRegion,
    message: u32,
}

/// Binds the mailbox's page (0x10_7c01_3000) and a DMA region for its
/// buffer, both consumed.
///
/// # Errors
///
/// Any error of viewing the page; [`HwError::Mailbox`] if the buffer is
/// smaller than a message, or its physical address is not 16-byte aligned
/// or not below 4 GiB.
pub fn mailbox(regs: Device, buffer: DmaRegion) -> Result<Mailbox, HwError> {
    let regs = regs.view_words()?;
    let address = buffer.device_address(0, MESSAGE_BYTES)?;
    let message = u32::try_from(address)
        .ok()
        .filter(|address| address.is_multiple_of(16))
        .ok_or(HwError::Mailbox {
            reason: "the buffer is not 16-byte aligned below 4 GiB",
        })?;
    Ok(Mailbox {
        regs,
        buffer,
        message: message | PROPERTY,
    })
}

impl Mailbox {
    /// Asks the firmware for the SoC's temperature, in thousandths of a
    /// degree.
    ///
    /// # Errors
    ///
    /// [`HwError::Mailbox`] if the mailbox stays full or the firmware does
    /// not answer within 100 ms, or its answer is refused
    /// ([`parse_temperature`]).
    pub fn temperature(&mut self) -> Result<i32, HwError> {
        let mut bytes = [0_u8; MESSAGE_BYTES];
        for (chunk, word) in bytes.chunks_exact_mut(4).zip(temperature_request()) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        self.buffer.write(0, &bytes)?;
        cpu::dsb();
        for _ in 0..DRAIN_AT_MOST {
            if read(&mut self.regs, READ_STATUS)? & EMPTY != 0 {
                break;
            }
            read(&mut self.regs, READ)?;
        }
        let deadline = cpu::counter()
            .saturating_add(cpu::counter_frequency().saturating_mul(ANSWER_WITHIN_MS) / 1000);
        while read(&mut self.regs, WRITE_STATUS)? & FULL != 0 {
            if cpu::counter() > deadline {
                return Err(HwError::Mailbox {
                    reason: "mailbox 1 stayed full",
                });
            }
        }
        write(&mut self.regs, WRITE, self.message)?;
        loop {
            if read(&mut self.regs, READ_STATUS)? & EMPTY == 0
                && read(&mut self.regs, READ)? == self.message
            {
                break;
            }
            if cpu::counter() > deadline {
                return Err(HwError::Mailbox {
                    reason: "the firmware did not answer within 100 ms",
                });
            }
        }
        cpu::dsb();
        self.buffer.read(0, &mut bytes)?;
        let mut answer = [0_u32; MESSAGE_WORDS];
        for (word, chunk) in answer.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = chunk.try_into().map(u32::from_le_bytes).unwrap_or_default();
        }
        parse_temperature(&answer)
    }
}

fn word(at: usize) -> Result<usize, HwError> {
    if at.is_multiple_of(4) {
        Ok(at / 4)
    } else {
        Err(HwError::Mailbox {
            reason: "a register off a word",
        })
    }
}

fn read(regs: &mut UniqueMmioPointer<'static, [u32]>, at: usize) -> Result<u32, HwError> {
    let mut register = regs.get(word(at)?).ok_or(HwError::Mailbox {
        reason: "the mailbox's page is too small",
    })?;
    // SAFETY: `read_unsafe` needs a read that is safe to make: inside the
    // page (`get` checked the index), of mailbox 0's or 1's STATUS, which
    // change nothing when read, or mailbox 0's READ, which pops a message
    // the firmware sent this PD - the only PD that holds the mailbox - and
    // touches no memory.
    Ok(unsafe { register.read_unsafe() })
}

fn write(
    regs: &mut UniqueMmioPointer<'static, [u32]>,
    at: usize,
    value: u32,
) -> Result<(), HwError> {
    let mut register = regs.get(word(at)?).ok_or(HwError::Mailbox {
        reason: "the mailbox's page is too small",
    })?;
    // SAFETY: `write_unsafe` needs a write that is safe to make: inside the
    // page (`get`), and only ever mailbox 1's WRITE with this PD's own
    // buffer's address - checked inside its DMA region by `device_address` -
    // holding nothing but the get-temperature request, which the firmware
    // answers in that buffer and nowhere else.
    unsafe { register.write_unsafe(value) };
    Ok(())
}

// The parser runs under Miri too; the register accesses (safe-mmio's inline
// assembly) only on the host, with make pds-test.
#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "test code: a failed check panics, and tests index known data"
    )]

    use super::*;

    fn answered(value: u32) -> [u32; MESSAGE_WORDS] {
        [
            32,
            ANSWERED,
            GET_TEMPERATURE,
            8,
            TAG_ANSWERED | 8,
            0,
            value,
            0,
        ]
    }

    #[test]
    fn the_request_is_get_temperature_for_sensor_0() {
        assert_eq!(temperature_request(), [32, 0, 0x0003_0006, 8, 0, 0, 0, 0]);
    }

    #[test]
    fn an_answered_temperature_is_read() {
        assert_eq!(parse_temperature(&answered(47_250)).unwrap(), 47_250);
    }

    #[test]
    fn anything_else_is_refused() {
        let mut unprocessed = answered(47_250);
        unprocessed[1] = 0;
        assert!(parse_temperature(&unprocessed).is_err(), "not processed");
        assert!(
            parse_temperature(&temperature_request()).is_err(),
            "the request itself"
        );
        let mut error = answered(47_250);
        error[1] = 0x8000_0001;
        assert!(parse_temperature(&error).is_err(), "a parsing error");
        for (index, wrong) in [
            (2, 0x0003_000a),
            (3, 4),
            (4, TAG_ANSWERED | 4),
            (5, 1),
            (7, 9),
        ] {
            let mut answer = answered(47_250);
            answer[index] = wrong;
            assert!(parse_temperature(&answer).is_err(), "word {index}");
        }
        assert!(
            parse_temperature(&answered(u32::MAX)).is_err(),
            "out of range"
        );
    }

    #[cfg(not(miri))]
    #[test]
    fn a_firmware_that_never_answers_times_out_and_names_this_buffer() {
        use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

        // Mailbox 0 empty, mailbox 1 not full: asked, never answered.
        let page: &'static [AtomicU32] = Vec::leak(
            (0..1024)
                .map(|at| AtomicU32::new(if at * 4 == READ_STATUS { EMPTY } else { 0 }))
                .collect(),
        );
        let bytes: &'static [AtomicU8] = Vec::leak((0..4096).map(|_| AtomicU8::new(0)).collect());
        let regs = Device::for_test(page.as_ptr().expose_provenance(), 4096);
        let mut mailbox = mailbox(regs, DmaRegion::for_test(bytes, 0x3a00_0000)).unwrap();
        assert!(matches!(
            mailbox.temperature(),
            Err(HwError::Mailbox { .. })
        ));
        assert_eq!(page[WRITE / 4].load(Ordering::Relaxed), 0x3a00_0008);
        assert_eq!(
            bytes[8].load(Ordering::Relaxed),
            0x06,
            "the request in the buffer"
        );
    }

    #[cfg(not(miri))]
    #[test]
    fn a_buffer_off_16_bytes_or_above_4_gib_is_refused() {
        use core::sync::atomic::{AtomicU8, AtomicU32};

        for paddr in [0x3a00_0004, 0x1_0000_0000] {
            let page: &'static [AtomicU32] =
                Vec::leak((0..1024).map(|_| AtomicU32::new(0)).collect());
            let bytes: &'static [AtomicU8] = Vec::leak((0..64).map(|_| AtomicU8::new(0)).collect());
            let regs = Device::for_test(page.as_ptr().expose_provenance(), 4096);
            assert!(
                mailbox(regs, DmaRegion::for_test(bytes, paddr)).is_err(),
                "{paddr:#x}"
            );
        }
    }
}
