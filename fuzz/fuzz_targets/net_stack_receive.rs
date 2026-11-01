// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: the network stack PD's receive path (`nowayos-ip`):
//! frames of the fuzzer's making passed back by a driver, through the
//! link's checks and copy and into smoltcp, whose parsing - Ethernet, ARP,
//! IPv4, UDP and DHCP - and DHCP client run on them.
//!
//! Each step is the driver's: a frame of any bytes and length in a buffer
//! the stack gave it; a DHCP reply - any address offered and any options,
//! most often after the usual ones (a message type, the server, a netmask,
//! a router and a lease), behind a BOOTP header carrying the transaction id
//! of the stack's last request, so the fuzzer reaches the DHCP client's
//! state machine, leases taken, renewed and lost included, and not only its
//! parser; a descriptor of its own making, of any offset and length; or
//! time passing. The stack is polled after each, as its PD
//! polls it, and every frame it sends is given back. (The fuzz build skips
//! smoltcp's checksum checks - `cfg(fuzzing)` - so every frame reaches the
//! parsers past them.)
//!
//! Checks: nothing panics; every frame the stack sends is a whole Ethernet
//! frame from its own MAC address; every receive buffer the driver passed
//! back went straight back to it, and none was given to it twice; and the
//! link counts every descriptor it refused.

#![no_main]

use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::sync::atomic::AtomicU8;

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_ip::{Link, Queues, RxMemory, Stack, StackSeed, TxMemory, storage};
use nowayos_net::arp::Mac;
use nowayos_net::{
    BUFFER_SIZE, Consumer, Descriptor, ETHERNET_HEADER_LEN, InMemoryQueue, MAX_FRAME_LEN, Producer,
};
use nowayos_time::Instant;

/// Buffers in each direction: few, so a full queue is common.
const N: usize = 8;
/// The most steps one input takes.
const MAX_STEPS: usize = 64;
/// The stack's MAC address.
const MAC: Mac = Mac([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
/// A millisecond and a second in nanoseconds.
const MS: u64 = 1_000_000;
const SECOND: u64 = 1_000_000_000;
/// Each direction's buffers, in bytes.
const REGION_LEN: usize = N.saturating_mul(BUFFER_SIZE);
/// The longest frame or wait one step asks for, and one more.
const LEN_LIMIT: usize = BUFFER_SIZE.saturating_add(1);
/// A BOOTP header's length, before the options.
const BOOTP_LEN: usize = 240;
/// Ethernet, IPv4 and UDP headers before a BOOTP message.
const HEADERS_LEN: usize = 42;

type Queue = InMemoryQueue<N>;

/// The transaction id of the last DHCP message in `frame`, if it is one the
/// stack sent: IPv4, UDP to port 67, a BOOTP request.
fn request_xid(frame: &[u8]) -> Option<[u8; 4]> {
    if frame.get(12..14)? != [0x08, 0x00] || *frame.get(23)? != 17 {
        return None;
    }
    let header_len = usize::from(frame.get(14)? & 0x0f).saturating_mul(4);
    let udp = frame.get(14usize.saturating_add(header_len)..)?;
    if udp.get(2..4)? != [0, 67] {
        return None;
    }
    udp.get(12..16)?.try_into().ok()
}

/// A DHCP reply to the stack's transaction `xid`, offering `yiaddr`:
/// `options` behind the Ethernet, IPv4, UDP and BOOTP headers a server
/// would send, cut to fit a frame. Checksums are left zero: the fuzz build
/// does not check them.
fn dhcp_reply(
    xid: [u8; 4],
    yiaddr: [u8; 4],
    options: &[u8],
    out: &mut [u8; MAX_FRAME_LEN],
) -> usize {
    let fixed = HEADERS_LEN.saturating_add(BOOTP_LEN);
    let options = options
        .get(..MAX_FRAME_LEN.saturating_sub(fixed))
        .unwrap_or(options);
    let len = fixed.saturating_add(options.len());
    out.fill(0);
    let ip_len = u16::try_from(len.saturating_sub(14)).unwrap_or(u16::MAX);
    let udp_len = u16::try_from(len.saturating_sub(34)).unwrap_or(u16::MAX);
    let mut write = |at: usize, bytes: &[u8]| {
        if let Some(to) = out.get_mut(at..at.saturating_add(bytes.len())) {
            to.copy_from_slice(bytes);
        }
    };
    write(0, &MAC.0);
    write(6, &[0x52, 0x55, 0x0a, 0x00, 0x02, 0x02]);
    write(12, &[0x08, 0x00, 0x45, 0x00]);
    write(16, &ip_len.to_be_bytes());
    write(22, &[64, 17]);
    write(26, &[10, 0, 2, 2, 255, 255, 255, 255]);
    write(34, &[0, 67, 0, 68]);
    write(38, &udp_len.to_be_bytes());
    // BOOTP: a reply, Ethernet, the stack's transaction, the address
    // offered, the server's, the stack's MAC address and the magic cookie;
    // then the options.
    write(42, &[2, 1, 6, 0]);
    write(46, &xid);
    write(58, &yiaddr);
    write(62, &[10, 0, 2, 2]);
    write(70, &MAC.0);
    write(278, &[99, 130, 83, 99]);
    write(fixed, options);
    len
}

/// The options a server sends with a reply of `message_type` (option 53):
/// its identifier, a netmask, a router and a lease of `lease` seconds - so
/// the fuzzer reaches offers, acknowledgements and naks, and leases taken,
/// renewed and lost, from its first inputs.
fn usual_options(message_type: u8, netmask: [u8; 4], lease: [u8; 4]) -> Vec<u8> {
    let mut options = vec![53, 1, message_type];
    options.extend_from_slice(&[54, 4, 10, 0, 2, 2, 1, 4]);
    options.extend_from_slice(&netmask);
    options.extend_from_slice(&[3, 4, 10, 0, 2, 2, 51, 4]);
    options.extend_from_slice(&lease);
    options
}

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);
    let queues = [Queue::new(), Queue::new(), Queue::new(), Queue::new()];
    let [rx_free_q, rx_active_q, tx_free_q, tx_active_q] = &queues;
    let rx: Vec<AtomicU8> = (0..REGION_LEN).map(|_| AtomicU8::new(0)).collect();
    let tx: Vec<AtomicU8> = (0..REGION_LEN).map(|_| AtomicU8::new(0)).collect();
    let Ok(mut link) = Link::<_, _, _, N>::new(
        Queues {
            rx_free: rx_free_q,
            rx_active: rx_active_q,
            tx_free: tx_free_q,
            tx_active: tx_active_q,
        },
        &rx[..],
        &tx[..],
    ) else {
        return;
    };
    let seed =
        StackSeed::from_non_zero(NonZeroU64::new(input.u32().into()).unwrap_or(NonZeroU64::MIN));
    let mut room = storage();
    let Ok(mut stack) = Stack::new(MAC, seed, &mut link, &mut room, Instant::from_nanos(0)) else {
        return;
    };
    link.start();
    let (Ok(mut rx_free), Ok(mut rx_active), Ok(mut tx_free), Ok(mut tx_active)) = (
        Consumer::new(rx_free_q),
        Producer::new(rx_active_q),
        Producer::new(tx_free_q),
        Consumer::new(tx_active_q),
    ) else {
        return;
    };
    let mut driver_has = BTreeSet::new();
    let mut xid = [0u8; 4];
    let mut now = 0u64;
    let mut refused = 0u32;
    let mut frame = [0u8; MAX_FRAME_LEN];
    for _ in 0..MAX_STEPS {
        if input.is_empty() {
            break;
        }
        while let Ok(Some(descriptor)) = rx_free.dequeue() {
            assert!(
                driver_has.insert(descriptor.io_or_offset),
                "a buffer given twice"
            );
        }
        let kind = input.byte() % 4;
        let len = usize::from(input.u16()) % LEN_LIMIT;
        match kind {
            0 | 1 => {
                let Some(offset) = driver_has.pop_first() else {
                    continue;
                };
                let body: Vec<u8> = if kind == 0 {
                    (0..len).map(|_| input.byte()).collect()
                } else {
                    let yiaddr: [u8; 4] = input.array();
                    let message_type = input.byte() % 8;
                    let netmask: [u8; 4] = input.array();
                    let lease: [u8; 4] = input.array();
                    let mut options = Vec::new();
                    if !input.byte().is_multiple_of(4) {
                        options.extend_from_slice(&usual_options(message_type, netmask, lease));
                    }
                    options.extend((0..len % 128).map(|_| input.byte()));
                    let len = dhcp_reply(xid, yiaddr, &options, &mut frame);
                    frame.get(..len).unwrap_or_default().to_vec()
                };
                let mut memory = &rx[..];
                assert!(memory.store(usize::try_from(offset).unwrap_or(usize::MAX), &body));
                if !(ETHERNET_HEADER_LEN..=MAX_FRAME_LEN).contains(&body.len()) {
                    refused = refused.saturating_add(1);
                }
                let descriptor =
                    Descriptor::new(offset, u16::try_from(body.len()).unwrap_or(u16::MAX));
                assert!(
                    rx_active.enqueue(descriptor).is_ok(),
                    "a buffer of the driver's, queued once"
                );
            }
            2 => {
                let offset = u64::from_le_bytes(input.array());
                let len = input.u16();
                if rx_active.enqueue(Descriptor::new(offset, len)).is_ok() {
                    driver_has.remove(&offset);
                }
            }
            _ => {
                let seconds = u64::try_from(len).unwrap_or(u64::MAX);
                now = now.saturating_add(seconds.saturating_mul(SECOND));
            }
        }
        now = now.saturating_add(MS);
        let _ = stack.poll(&mut link, Instant::from_nanos(now));
        link.service();
        while let Ok(Some(descriptor)) = tx_active.dequeue() {
            let sent_len = usize::from(descriptor.len);
            assert!(
                (ETHERNET_HEADER_LEN..=MAX_FRAME_LEN).contains(&sent_len),
                "a {sent_len}-byte frame sent"
            );
            let mut sent = vec![0u8; sent_len];
            let at = usize::try_from(descriptor.io_or_offset).unwrap_or(usize::MAX);
            assert!(
                tx[..].load(at, &mut sent),
                "a frame outside the transmit buffers"
            );
            assert_eq!(
                sent.get(6..12),
                Some(&MAC.0[..]),
                "a frame not from the stack's MAC address"
            );
            if let Some(id) = request_xid(&sent) {
                xid = id;
            }
            let _ = tx_free.enqueue(Descriptor::new(descriptor.io_or_offset, 0));
        }
        assert_eq!(link.held().0, 0, "a buffer passed back kept");
    }
    let counts = link.counts();
    assert!(
        counts.oversize.saturating_add(counts.malformed) >= refused,
        "{refused} refused frames, {counts:?}"
    );
});
