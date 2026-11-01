// SPDX-License-Identifier: MIT OR Apache-2.0
//! The GEM's receive and transmit descriptor rings, in 64-bit addressing
//! mode: four 32-bit words a descriptor (UG1085 tables 34-5 to 34-9).
//!
//! **Receive** (table 34-5): word 0 holds the buffer's address (bits 31:3,
//! and 2 outside extended mode), the wrap bit (1) and ownership (0: zero
//! lets the MAC write the buffer; the MAC sets it once a frame is in
//! memory); word 1 the frame's status, its length in bits 12:0, start of
//! frame in 14 and end of frame in 15; word 2 the address's high 32 bits;
//! word 3 is unused. **Transmit** (table 34-8): word 0 the address, word 1
//! the length (13:0), last buffer (15), wrap (30) and used (31: zero hands
//! it to the MAC, which sets it once the frame is sent), and its error bits
//! (29, 27, 26); word 2 the address's high bits.
//!
//! Both rings are FIFOs in ring order, as the MAC walks them: the driver
//! hands buffers over at the tail and takes them back at the head, and
//! keeps its own record of which buffer each slot holds and at which bus
//! address. The MAC writes the descriptors, so everything read back is a
//! copy, checked before use:
//!
//! * a received frame must be whole in its buffer (start and end of frame
//!   both set), [`MIN_FRAME_LEN`] to [`MAX_FRAME_LEN`] bytes (no jumbo
//!   bit), and its descriptor must still hold the address the driver wrote.
//!   One that fails is dropped and counted - [`Drop::Oversize`],
//!   [`Drop::Malformed`], [`Drop::Moved`] - and its buffer comes back to
//!   the driver to offer again;
//! * the buffer handed back is always the driver's record of the slot,
//!   never an address the MAC wrote, so no write can make a ring hand out a
//!   buffer it was not given, or one twice;
//! * a transmit descriptor's error bits are counted.
//!
//! Buffers are the client's data region's, by offset ([`nowayos_net`]'s
//! `DataRegion` checks them before they get here); the bus address the
//! driver computes for each must be 8-byte aligned.

use nowayos_net::{ETHERNET_HEADER_LEN, MAX_FRAME_LEN as NET_MAX_FRAME_LEN};

/// The bytes of one descriptor: four words.
pub const DESCRIPTOR_LEN: usize = 16;

/// The shortest frame passed on: an Ethernet header.
pub const MIN_FRAME_LEN: usize = ETHERNET_HEADER_LEN;

/// The longest frame passed on or sent: an Ethernet header and 1,500 bytes
/// (the MAC removes the FCS).
pub const MAX_FRAME_LEN: usize = NET_MAX_FRAME_LEN;

/// Receive word 0: ownership. Set, the buffer is the driver's.
const RX_OWNED_BY_DRIVER: u32 = 1 << 0;
/// Receive word 0: the last descriptor of the ring.
const RX_WRAP: u32 = 1 << 1;
/// Receive word 0: the flags; the rest is the address.
const RX_FLAGS: u32 = 0b11;
/// Receive word 1: end of frame.
const RX_EOF: u32 = 1 << 15;
/// Receive word 1: start of frame.
const RX_SOF: u32 = 1 << 14;
/// Receive word 1: a jumbo frame's length bit, or a bad FCS when it is
/// ignored: neither mode is on, so it must be clear.
const RX_LENGTH_13: u32 = 1 << 13;
/// Receive word 1: the frame's length.
const RX_LENGTH: u32 = 0x1FFF;

/// Transmit word 1: used. Set, the buffer is the driver's.
const TX_USED: u32 = 1 << 31;
/// Transmit word 1: the last descriptor of the ring.
const TX_WRAP: u32 = 1 << 30;
/// Transmit word 1: retry limit exceeded.
const TX_RETRY_LIMIT: u32 = 1 << 29;
/// Transmit word 1: frame corrupted by an AXI error.
const TX_AXI_ERROR: u32 = 1 << 27;
/// Transmit word 1: late collision.
const TX_LATE_COLLISION: u32 = 1 << 26;
/// Transmit word 1: the last buffer of the frame.
const TX_LAST: u32 = 1 << 15;

/// The memory a ring's descriptors are in: the driver's DMA region, which
/// the MAC also writes.
pub trait DescriptorMemory {
    /// The word at byte `offset`, copied, or `None` outside the memory.
    fn read_word(&self, offset: usize) -> Option<u32>;
    /// Writes the word at byte `offset`; `None` outside the memory.
    fn write_word(&mut self, offset: usize, value: u32) -> Option<()>;
    /// Makes the writes before it visible to the MAC before those after
    /// it (a memory barrier on the hardware; nothing in a test).
    fn barrier(&self);
}

/// Why a ring operation failed: the driver's mistake, never the MAC's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    /// A descriptor outside the descriptor memory.
    Memory,
    /// A bus address not 8-byte aligned.
    Address,
    /// A frame to send of a length out of range.
    Length,
}

/// Why a received frame was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    /// Longer than [`MAX_FRAME_LEN`].
    Oversize,
    /// Not whole in one buffer, shorter than [`MIN_FRAME_LEN`], or with
    /// the jumbo or bad-FCS bit set.
    Malformed,
    /// Its descriptor no longer holds the address the driver wrote.
    Moved,
}

/// What [`RxRing::take`] found at the head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// A frame that passed its checks: its buffer and length.
    Frame {
        /// The buffer, as the driver offered it.
        buffer: usize,
        /// The frame's length.
        len: usize,
    },
    /// A frame dropped and counted; its buffer is the driver's again.
    Dropped {
        /// The buffer, as the driver offered it.
        buffer: usize,
        /// Why.
        why: Drop,
    },
}

/// A buffer a slot holds: the driver's record, never the MAC's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    buffer: usize,
    address: u64,
}

/// How many slots ahead of `index`, round a ring of `n`.
const fn next(index: usize, n: usize) -> usize {
    match index.checked_add(1) {
        Some(after) if after < n => after,
        _ => 0,
    }
}

/// The byte offset of word `word` of descriptor `slot` of a ring at `base`.
fn word_offset(base: usize, slot: usize, word: usize) -> Option<usize> {
    slot.checked_mul(DESCRIPTOR_LEN)
        .and_then(|at| at.checked_add(word.checked_mul(4)?))
        .and_then(|at| base.checked_add(at))
}

/// The low and high words of a bus address, if it is 8-byte aligned.
fn split(address: u64) -> Result<(u32, u32), RingError> {
    if address & 0b111 != 0 {
        return Err(RingError::Address);
    }
    Ok(((address & 0xFFFF_FFFF) as u32, (address >> 32) as u32))
}

/// The receive ring: `N` descriptors at byte `base` of the descriptor
/// memory.
#[derive(Debug)]
pub struct RxRing<const N: usize> {
    base: usize,
    slots: [Option<Held>; N],
    head: usize,
    tail: usize,
    oversize: u32,
    malformed: u32,
    moved: u32,
}

impl<const N: usize> RxRing<N> {
    /// The ring's size in bytes.
    pub const BYTES: usize = N * DESCRIPTOR_LEN;

    /// A ring at byte `base`, holding nothing. Call [`init`](Self::init)
    /// before the MAC is given its address.
    #[must_use]
    pub const fn new(base: usize) -> Self {
        Self {
            base,
            slots: [None; N],
            head: 0,
            tail: 0,
            oversize: 0,
            malformed: 0,
            moved: 0,
        }
    }

    /// Writes every descriptor as the driver's, with no buffer, the last
    /// marked to wrap: the MAC writes nothing until buffers are offered.
    ///
    /// # Errors
    ///
    /// [`RingError::Memory`] if the ring does not fit the memory.
    pub fn init(&mut self, memory: &mut impl DescriptorMemory) -> Result<(), RingError> {
        for slot in 0..N {
            for word in 1..4 {
                write(memory, self.base, slot, word, 0)?;
            }
            write(
                memory,
                self.base,
                slot,
                0,
                RX_OWNED_BY_DRIVER | wrap(slot, N, RX_WRAP),
            )?;
        }
        *self = Self::new(self.base);
        Ok(())
    }

    /// How many buffers the MAC holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Hands `buffer`, at bus address `address`, to the MAC at the tail.
    /// `Ok(false)` if every slot holds one already.
    ///
    /// # Errors
    ///
    /// [`RingError::Address`] for an address not 8-byte aligned;
    /// [`RingError::Memory`] if the ring does not fit the memory.
    pub fn offer(
        &mut self,
        memory: &mut impl DescriptorMemory,
        buffer: usize,
        address: u64,
    ) -> Result<bool, RingError> {
        let slot = self.tail;
        if self.slots.get(slot).is_none_or(Option::is_some) {
            return Ok(false);
        }
        let (low, high) = split(address)?;
        write(memory, self.base, slot, 2, high)?;
        write(memory, self.base, slot, 1, 0)?;
        // The address first, then ownership to the MAC.
        memory.barrier();
        write(memory, self.base, slot, 0, low | wrap(slot, N, RX_WRAP))?;
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = Some(Held { buffer, address });
        }
        self.tail = next(slot, N);
        Ok(true)
    }

    /// The frame at the head, once the MAC has written it: checked, and its
    /// buffer the driver's again. `Ok(None)` while the MAC still owns it,
    /// or no buffer is there.
    ///
    /// # Errors
    ///
    /// [`RingError::Memory`] if the ring does not fit the memory.
    pub fn take(
        &mut self,
        memory: &mut impl DescriptorMemory,
    ) -> Result<Option<Received>, RingError> {
        let slot = self.head;
        let Some(Some(held)) = self.slots.get(slot).copied() else {
            return Ok(None);
        };
        let word0 = read(memory, self.base, slot, 0)?;
        if word0 & RX_OWNED_BY_DRIVER == 0 {
            return Ok(None);
        }
        let word1 = read(memory, self.base, slot, 1)?;
        let word2 = read(memory, self.base, slot, 2)?;
        let (low, high) = split(held.address)?;
        let len = (word1 & RX_LENGTH) as usize;
        let received = if word0 & !RX_FLAGS != low || word2 != high {
            self.moved = self.moved.saturating_add(1);
            Received::Dropped {
                buffer: held.buffer,
                why: Drop::Moved,
            }
        } else if word1 & (RX_SOF | RX_EOF) != RX_SOF | RX_EOF
            || word1 & RX_LENGTH_13 != 0
            || len < MIN_FRAME_LEN
        {
            self.malformed = self.malformed.saturating_add(1);
            Received::Dropped {
                buffer: held.buffer,
                why: Drop::Malformed,
            }
        } else if len > MAX_FRAME_LEN {
            self.oversize = self.oversize.saturating_add(1);
            Received::Dropped {
                buffer: held.buffer,
                why: Drop::Oversize,
            }
        } else {
            Received::Frame {
                buffer: held.buffer,
                len,
            }
        };
        // The slot is the driver's again, empty.
        write(memory, self.base, slot, 1, 0)?;
        write(
            memory,
            self.base,
            slot,
            0,
            RX_OWNED_BY_DRIVER | wrap(slot, N, RX_WRAP),
        )?;
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = None;
        }
        self.head = next(slot, N);
        Ok(Some(received))
    }

    /// Frames dropped as longer than [`MAX_FRAME_LEN`].
    #[must_use]
    pub const fn oversize(&self) -> u32 {
        self.oversize
    }

    /// Frames dropped as malformed.
    #[must_use]
    pub const fn malformed(&self) -> u32 {
        self.malformed
    }

    /// Descriptors dropped because their address had changed.
    #[must_use]
    pub const fn moved(&self) -> u32 {
        self.moved
    }
}

/// What [`TxRing::reclaim`] got back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The buffer, as the driver sent it.
    pub buffer: usize,
    /// Whether the MAC reported it sent without error.
    pub ok: bool,
}

/// The transmit ring: `N` descriptors at byte `base` of the descriptor
/// memory.
#[derive(Debug)]
pub struct TxRing<const N: usize> {
    base: usize,
    slots: [Option<usize>; N],
    head: usize,
    tail: usize,
    errors: u32,
}

impl<const N: usize> TxRing<N> {
    /// The ring's size in bytes.
    pub const BYTES: usize = N * DESCRIPTOR_LEN;

    /// A ring at byte `base`, holding nothing. Call [`init`](Self::init)
    /// before the MAC is given its address.
    #[must_use]
    pub const fn new(base: usize) -> Self {
        Self {
            base,
            slots: [None; N],
            head: 0,
            tail: 0,
            errors: 0,
        }
    }

    /// Writes every descriptor as the driver's (used), the last marked to
    /// wrap: the MAC sends nothing until frames are queued.
    ///
    /// # Errors
    ///
    /// [`RingError::Memory`] if the ring does not fit the memory.
    pub fn init(&mut self, memory: &mut impl DescriptorMemory) -> Result<(), RingError> {
        for slot in 0..N {
            write(memory, self.base, slot, 0, 0)?;
            write(memory, self.base, slot, 2, 0)?;
            write(memory, self.base, slot, 3, 0)?;
            write(memory, self.base, slot, 1, TX_USED | wrap(slot, N, TX_WRAP))?;
        }
        *self = Self::new(self.base);
        Ok(())
    }

    /// How many frames the MAC holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Queues the `len`-byte frame in `buffer`, at bus address `address`,
    /// for the MAC at the tail. `Ok(false)` if every slot holds one
    /// already. The driver then starts transmission (`network_control`'s
    /// `tx_start_pclk`).
    ///
    /// # Errors
    ///
    /// [`RingError::Length`] for a length outside [`MIN_FRAME_LEN`] to
    /// [`MAX_FRAME_LEN`]; [`RingError::Address`] for an address not 8-byte
    /// aligned; [`RingError::Memory`] if the ring does not fit the memory.
    pub fn send(
        &mut self,
        memory: &mut impl DescriptorMemory,
        buffer: usize,
        address: u64,
        len: usize,
    ) -> Result<bool, RingError> {
        if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&len) {
            return Err(RingError::Length);
        }
        let slot = self.tail;
        if self.slots.get(slot).is_none_or(Option::is_some) {
            return Ok(false);
        }
        let (low, high) = split(address)?;
        write(memory, self.base, slot, 0, low)?;
        write(memory, self.base, slot, 2, high)?;
        // The address first, then the length and the hand-over.
        memory.barrier();
        let len = u32::try_from(len).map_err(|_| RingError::Length)?;
        write(
            memory,
            self.base,
            slot,
            1,
            len | TX_LAST | wrap(slot, N, TX_WRAP),
        )?;
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = Some(buffer);
        }
        self.tail = next(slot, N);
        Ok(true)
    }

    /// The frame at the head, once the MAC has sent it: its buffer the
    /// driver's again, and whether it went without error (the error bits
    /// are counted). `Ok(None)` while the MAC still holds it, or nothing is
    /// queued.
    ///
    /// # Errors
    ///
    /// [`RingError::Memory`] if the ring does not fit the memory.
    pub fn reclaim(
        &mut self,
        memory: &mut impl DescriptorMemory,
    ) -> Result<Option<Sent>, RingError> {
        let slot = self.head;
        let Some(Some(buffer)) = self.slots.get(slot).copied() else {
            return Ok(None);
        };
        let word1 = read(memory, self.base, slot, 1)?;
        if word1 & TX_USED == 0 {
            return Ok(None);
        }
        let ok = word1 & (TX_RETRY_LIMIT | TX_AXI_ERROR | TX_LATE_COLLISION) == 0;
        if !ok {
            self.errors = self.errors.saturating_add(1);
        }
        write(memory, self.base, slot, 1, TX_USED | wrap(slot, N, TX_WRAP))?;
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = None;
        }
        self.head = next(slot, N);
        Ok(Some(Sent { buffer, ok }))
    }

    /// Frames the MAC reported an error sending.
    #[must_use]
    pub const fn errors(&self) -> u32 {
        self.errors
    }

    /// The buffer of the frame at the head, given up whether the MAC sent
    /// it or not, and its slot the driver's again: for when the driver has
    /// turned the transmitter off, which returns the MAC to the ring's base
    /// (UG1087, `enable_transmit`), so the ring is emptied and set up again
    /// ([`init`](Self::init)) before it is turned back on. `Ok(None)` once
    /// nothing is queued.
    ///
    /// # Errors
    ///
    /// [`RingError::Memory`] if the ring does not fit the memory.
    pub fn abandon(
        &mut self,
        memory: &mut impl DescriptorMemory,
    ) -> Result<Option<usize>, RingError> {
        let slot = self.head;
        let Some(Some(buffer)) = self.slots.get(slot).copied() else {
            return Ok(None);
        };
        write(memory, self.base, slot, 1, TX_USED | wrap(slot, N, TX_WRAP))?;
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = None;
        }
        self.head = next(slot, N);
        Ok(Some(buffer))
    }
}

/// `flag` if `slot` is a ring of `n`'s last.
const fn wrap(slot: usize, n: usize, flag: u32) -> u32 {
    if slot.saturating_add(1) == n { flag } else { 0 }
}

fn read(
    memory: &impl DescriptorMemory,
    base: usize,
    slot: usize,
    word: usize,
) -> Result<u32, RingError> {
    word_offset(base, slot, word)
        .and_then(|at| memory.read_word(at))
        .ok_or(RingError::Memory)
}

fn write(
    memory: &mut impl DescriptorMemory,
    base: usize,
    slot: usize,
    word: usize,
    value: u32,
) -> Result<(), RingError> {
    word_offset(base, slot, word)
        .and_then(|at| memory.write_word(at, value))
        .ok_or(RingError::Memory)
}
