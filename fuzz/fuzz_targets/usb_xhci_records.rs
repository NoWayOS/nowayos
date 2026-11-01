// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fuzz target: what an xHCI controller writes and what its registers say,
//! as `nowayos-usb` decodes them for the USB host PD.
//!
//! The bytes are, in turn: an event ring record, as the controller writes
//! it into DMA memory and the driver copies it out; the eight capability
//! registers' words and a window size, as a controller's window gives them;
//! a Supported Protocol capability's three words; a PORTSC value; and an
//! output slot context. Checks:
//!
//! * `Event::decode` never panics, and what it accepts keeps its rules: a
//!   transfer names an endpoint from 1 to 31 and a slot from 1, every TRB
//!   pointer is 16-byte aligned, a port status change names a port from 1,
//!   and only events of the kinds it lists come back as ignored;
//! * capability registers that decode give a layout whose every register -
//!   each port's PORTSC, the operational registers, interrupter 0's and the
//!   doorbells - lies inside the window, or are refused for it;
//! * a Supported Protocol that decodes names ports 1 to 255 only;
//! * no PORTSC write value ever sets Port Enabled (a write of 1 disables
//!   the port), and none clears a change bit but a ClearChanges;
//! * a slot context decodes, or not, without a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nowayos_fuzz::Input;
use nowayos_usb::caps::{CAPABILITY_WORDS, Capabilities, Protocol};
use nowayos_usb::context::SlotReport;
use nowayos_usb::port::{PortStatus, PortWrite};
use nowayos_usb::trb::{Event, TRB_LEN};

fuzz_target!(|data: &[u8]| {
    let input = Input::new(data);

    let record: [u8; TRB_LEN] = input.array();
    match Event::decode(&record) {
        Ok(Event::Transfer {
            trb,
            endpoint,
            slot,
            residual,
            ..
        }) => {
            assert!(trb % 16 == 0 && (1..=31).contains(&endpoint) && slot != 0);
            assert!(residual < 1 << 24);
        }
        Ok(Event::CommandCompletion { trb, .. }) => assert!(trb % 16 == 0),
        Ok(Event::PortStatusChange { port }) => assert!(port != 0),
        Ok(Event::HostController { .. }) | Err(_) => {}
        Ok(Event::Ignored { kind }) => assert!(matches!(kind, 35 | 36 | 38 | 39)),
    }

    let mut words = [0u32; CAPABILITY_WORDS];
    for word in &mut words {
        *word = input.u32();
    }
    let window = usize::from(input.u16());
    if let Ok(caps) = Capabilities::decode(&words)
        && let Ok(layout) = caps.layout(window)
    {
        let inside = |at: Option<usize>| {
            at.is_none_or(|at| at.checked_add(4).is_some_and(|end| end <= window))
        };
        for port in 0..=u8::MAX {
            assert!(
                inside(layout.portsc(port)),
                "PORTSC {port} outside {window:#x}"
            );
        }
        for offset in (0..0x40).step_by(4) {
            assert!(inside(layout.operational(offset)));
        }
        for offset in (0..0x20).step_by(4) {
            assert!(inside(layout.interrupter(offset)));
        }
        for index in 0..=caps.max_slots {
            let doorbell = layout.doorbell(index, caps.max_slots);
            assert!(doorbell.is_some() && inside(doorbell));
        }
    }

    if let Some(protocol) = Protocol::decode([input.u32(), input.u32(), input.u32()]) {
        assert!(protocol.first_port >= 1 && protocol.ports >= 1);
        assert!(
            protocol
                .first_port
                .checked_add(protocol.ports.wrapping_sub(1))
                .is_some()
        );
    }

    let read = PortStatus(input.u32());
    for write in [PortWrite::Power, PortWrite::Reset, PortWrite::ClearChanges] {
        let value = read.write_value(write);
        assert_eq!(
            value & 0b10,
            0,
            "a PORTSC write that would disable the port"
        );
        if write != PortWrite::ClearChanges {
            assert_eq!(
                value & (0x7F << 17),
                0,
                "a PORTSC write that clears a change"
            );
        }
    }

    let _ = SlotReport::decode(input.rest());
});
