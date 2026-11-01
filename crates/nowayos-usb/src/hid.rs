// SPDX-License-Identifier: MIT OR Apache-2.0
//! HID boot-protocol reports turned into canonical input events (HID 1.11,
//! appendix B).
//!
//! A keyboard or mouse put in the boot protocol (`SET_PROTOCOL(0)`) sends
//! fixed reports that need no report descriptor:
//!
//! * **keyboard** (appendix B.1), 8 bytes: a modifier byte - bit *n* is
//!   usage `0xE0 + n`, left Control to right GUI - a reserved byte, then up
//!   to six key usages from the Keyboard/Keypad page, 0 for none. When more
//!   keys are down than the report holds, every key slot reads
//!   ErrorRollOver (1);
//! * **mouse** (appendix B.2), at least 3 bytes: buttons 1 to 3 in bits 0
//!   to 2, then X and Y displacements, signed bytes. Anything after them is
//!   the device's own and is ignored.
//!
//! A report is a state, not an event: [`Keyboard::report`] compares it with
//! the last one and makes a [`KeyDown`](InputEvent::KeyDown) for each key
//! newly down and a [`KeyUp`](InputEvent::KeyUp) for each released - every
//! release before any press. [`Mouse::report`] moves the pointer by the
//! displacement, clamped to the screen ([`PointerTracker`]), and reports
//! each button that changed. **The device is not trusted:** a report of the
//! wrong length, a rollover, an undefined usage - none is forwarded, each is
//! counted ([`Dropped`]), and none can panic.

use nowayos_input::{InputEvent, InputLimits, KeyCode, PointerTracker};

/// The bytes of a boot keyboard report.
pub const KEYBOARD_REPORT_LEN: usize = 8;

/// The fewest bytes of a boot mouse report.
pub const MOUSE_REPORT_MIN: usize = 3;

/// The most bytes of any boot report this driver takes: a full-speed
/// interrupt packet.
pub const REPORT_MAX: usize = 64;

/// The most events one keyboard report makes: eight modifiers changing, six
/// keys released and six pressed.
pub const MAX_KEYBOARD_EVENTS: usize = 20;

/// The most events one mouse report makes: a move and three buttons.
pub const MAX_MOUSE_EVENTS: usize = 4;

/// The usage ErrorRollOver: more keys down than the report holds.
const ROLLOVER: u8 = 0x01;
/// POSTFail and ErrorUndefined: the keyboard reports a fault.
const POST_FAIL: u8 = 0x02;
const UNDEFINED: u8 = 0x03;

/// What was refused, and how often.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// Reports of a length the boot protocol does not have.
    pub wrong_length: u32,
    /// Keyboard reports of rollover or a fault: the keys stay as they were.
    pub rollover: u32,
    /// Usages that are not defined keys: never forwarded.
    pub undefined_usage: u32,
}

impl Dropped {
    /// Everything dropped.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.wrong_length
            .saturating_add(self.rollover)
            .saturating_add(self.undefined_usage)
    }
}

/// A boot keyboard's state: the modifiers and keys its last report held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keyboard {
    modifiers: u8,
    keys: [u8; 6],
    dropped: Dropped,
}

impl Keyboard {
    /// A keyboard with nothing down.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            modifiers: 0,
            keys: [0; 6],
            dropped: Dropped {
                wrong_length: 0,
                rollover: 0,
                undefined_usage: 0,
            },
        }
    }

    /// What has been dropped so far.
    #[must_use]
    pub const fn dropped(&self) -> Dropped {
        self.dropped
    }

    /// Takes a copy of one report, and passes `emit` an event for each key
    /// it changes - releases first - at most [`MAX_KEYBOARD_EVENTS`].
    /// A report that is not 8 bytes, or that reports rollover or a fault,
    /// changes nothing.
    pub fn report(&mut self, report: &[u8], mut emit: impl FnMut(InputEvent)) {
        let Ok(report) = <[u8; KEYBOARD_REPORT_LEN]>::try_from(report) else {
            self.dropped.wrong_length = self.dropped.wrong_length.saturating_add(1);
            return;
        };
        let [modifiers, _reserved, k0, k1, k2, k3, k4, k5] = report;
        let keys = [k0, k1, k2, k3, k4, k5];
        if keys
            .iter()
            .any(|key| matches!(*key, ROLLOVER | POST_FAIL | UNDEFINED))
        {
            self.dropped.rollover = self.dropped.rollover.saturating_add(1);
            return;
        }
        // Releases: modifiers, then keys no longer down.
        for bit in 0..8u8 {
            let mask = 1u8 << bit;
            if self.modifiers & mask != 0
                && modifiers & mask == 0
                && let Some(key) = KeyCode::new(0xE0 | u16::from(bit))
            {
                emit(InputEvent::KeyUp(key));
            }
        }
        for (index, &old) in self.keys.iter().enumerate() {
            // Only what a press was reported for: a defined key, not a
            // modifier's usage in a key slot.
            if old != 0
                && !keys.contains(&old)
                && first(&self.keys, index, old)
                && let Some(key) = KeyCode::new(u16::from(old))
                && !key.is_modifier()
            {
                emit(InputEvent::KeyUp(key));
            }
        }
        // Presses: modifiers, then keys newly down, each once.
        for bit in 0..8u8 {
            let mask = 1u8 << bit;
            if self.modifiers & mask == 0
                && modifiers & mask != 0
                && let Some(key) = KeyCode::new(0xE0 | u16::from(bit))
            {
                emit(InputEvent::KeyDown(key));
            }
        }
        for (index, &new) in keys.iter().enumerate() {
            if new == 0 || self.keys.contains(&new) || !first(&keys, index, new) {
                continue;
            }
            match KeyCode::new(u16::from(new)) {
                // A modifier's usage in a key slot: the modifier byte is
                // what counts for those.
                Some(key) if !key.is_modifier() => emit(InputEvent::KeyDown(key)),
                _ => {
                    self.dropped.undefined_usage = self.dropped.undefined_usage.saturating_add(1);
                }
            }
        }
        self.modifiers = modifiers;
        self.keys = keys;
    }

    /// Releases everything held down - when the keyboard goes away - and
    /// forgets it.
    pub fn release_all(&mut self, emit: impl FnMut(InputEvent)) {
        self.report(&[0; KEYBOARD_REPORT_LEN], emit);
    }
}

/// How long a key is held before it first repeats: Windows' default delay.
pub const REPEAT_DELAY_US: u64 = 500_000;

/// How far apart its repeats come after that: about 30 a second, Windows'
/// default rate.
pub const REPEAT_EVERY_US: u64 = 33_333;

/// The most repeats one call to [`KeyRepeat::due`] gives: a tick that came
/// late catches up by a few, never a flood.
pub const MAX_REPEATS_AT_ONCE: u32 = 2;

/// The lock keys' usages (HID Usage Tables, keyboard page): Caps Lock,
/// Scroll Lock and Num Lock. A held lock key toggles once, so it never
/// repeats.
const LOCKS: [u16; 3] = [0x39, 0x47, 0x53];

/// A held key's repeats. A USB keyboard reports presses and
/// releases alone; the computer it is plugged into makes the repeats, as
/// virtio-input's device does for QEMU's. Fed each event a keyboard's
/// decoder makes ([`KeyRepeat::saw`]) and the time on each tick
/// ([`KeyRepeat::due`]), it gives the held key's press again after
/// [`REPEAT_DELAY_US`] and then every [`REPEAT_EVERY_US`] - the same event
/// as the first press - until the key is released, another key is pressed
/// (which then repeats in its place) or the keyboard goes
/// ([`KeyRepeat::stop`]). A modifier or a lock key never repeats, and
/// pressing a modifier does not stop a repeat: Shift held over a repeating
/// key changes what the host makes of it, as on any computer.
///
/// Times are microseconds on the caller's clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyRepeat {
    /// The key repeating, and when its next press is due.
    held: Option<(KeyCode, u64)>,
}

impl KeyRepeat {
    /// No key held.
    #[must_use]
    pub const fn new() -> Self {
        Self { held: None }
    }

    /// The key repeating, if one is.
    #[must_use]
    pub fn held(&self) -> Option<KeyCode> {
        self.held.map(|(key, _)| key)
    }

    /// An event the keyboard made at `now`: a press of a key that repeats
    /// starts its repeats; its release stops them.
    pub fn saw(&mut self, event: InputEvent, now: u64) {
        match event {
            InputEvent::KeyDown(key) if repeats(key) => {
                self.held = Some((key, now.saturating_add(REPEAT_DELAY_US)));
            }
            InputEvent::KeyUp(key) if self.held() == Some(key) => self.held = None,
            _ => {}
        }
    }

    /// The repeats due by `now`, each passed to `emit` as a press of the
    /// held key - at most [`MAX_REPEATS_AT_ONCE`]; one that falls further
    /// behind starts again from `now`.
    pub fn due(&mut self, now: u64, mut emit: impl FnMut(InputEvent)) {
        let Some((key, mut next)) = self.held else {
            return;
        };
        let mut given = 0u32;
        while next <= now && given < MAX_REPEATS_AT_ONCE {
            emit(InputEvent::KeyDown(key));
            given = given.saturating_add(1);
            next = next.saturating_add(REPEAT_EVERY_US);
        }
        if next <= now {
            next = now.saturating_add(REPEAT_EVERY_US);
        }
        self.held = Some((key, next));
    }

    /// The keyboard went, or everything it held was released: no more
    /// repeats.
    pub const fn stop(&mut self) {
        self.held = None;
    }
}

/// Whether `key` repeats when held: neither a modifier nor a lock key.
fn repeats(key: KeyCode) -> bool {
    !key.is_modifier() && !LOCKS.contains(&key.usage())
}

/// Whether `value` at `index` is its first appearance in `keys`.
fn first(keys: &[u8; 6], index: usize, value: u8) -> bool {
    keys.iter().position(|key| *key == value) == Some(index)
}

/// A boot mouse's state: its buttons, and the pointer it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    buttons: u8,
    pointer: PointerTracker,
    dropped: Dropped,
}

impl Mouse {
    /// A mouse with no buttons down, its pointer in the middle of the
    /// screen `limits` gives.
    #[must_use]
    pub fn new(limits: InputLimits) -> Self {
        Self {
            buttons: 0,
            pointer: PointerTracker::new(limits),
            dropped: Dropped::default(),
        }
    }

    /// What has been dropped so far.
    #[must_use]
    pub const fn dropped(&self) -> Dropped {
        self.dropped
    }

    /// Takes a copy of one report, and passes `emit` the pointer's new
    /// position if it moved, then each button that changed - at most
    /// [`MAX_MOUSE_EVENTS`]. A report shorter than 3 bytes or longer than
    /// [`REPORT_MAX`] changes nothing.
    pub fn report(&mut self, report: &[u8], mut emit: impl FnMut(InputEvent)) {
        let (Some(&buttons), Some(&dx), Some(&dy)) = (report.first(), report.get(1), report.get(2))
        else {
            self.dropped.wrong_length = self.dropped.wrong_length.saturating_add(1);
            return;
        };
        if report.len() > REPORT_MAX {
            self.dropped.wrong_length = self.dropped.wrong_length.saturating_add(1);
            return;
        }
        let (dx, dy) = (i32::from(dx as i8), i32::from(dy as i8));
        if dx != 0 || dy != 0 {
            emit(self.pointer.apply_delta(dx, dy));
        }
        self.set_buttons(buttons & 0x7, &mut emit);
    }

    /// Releases every button held down - when the mouse goes away.
    pub fn release_all(&mut self, mut emit: impl FnMut(InputEvent)) {
        self.set_buttons(0, &mut emit);
    }

    fn set_buttons(&mut self, buttons: u8, emit: &mut impl FnMut(InputEvent)) {
        for button in 0..3u8 {
            let mask = 1u8 << button;
            if (self.buttons ^ buttons) & mask != 0 {
                emit(InputEvent::PointerButton {
                    button,
                    down: buttons & mask != 0,
                });
            }
        }
        self.buttons = buttons;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failed check panics, and tests index and compute on known data"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn keyboard_events(keyboard: &mut Keyboard, report: &[u8]) -> Vec<InputEvent> {
        let mut events = Vec::new();
        keyboard.report(report, |event| events.push(event));
        events
    }

    fn key(usage: u16) -> KeyCode {
        KeyCode::new(usage).unwrap()
    }

    #[test]
    fn typing_shift_a() {
        let mut keyboard = Keyboard::new();
        // Left Shift down.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0x02, 0, 0, 0, 0, 0, 0, 0]),
            [InputEvent::KeyDown(KeyCode::LEFT_SHIFT)]
        );
        // A down, Shift still down.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]),
            [InputEvent::KeyDown(KeyCode::A)]
        );
        // The same report again: nothing.
        assert!(keyboard_events(&mut keyboard, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]).is_empty());
        // Both up at once: releases only.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0; 8]),
            [
                InputEvent::KeyUp(KeyCode::LEFT_SHIFT),
                InputEvent::KeyUp(KeyCode::A)
            ]
        );
    }

    #[test]
    fn releases_come_before_presses_and_keys_move_between_slots() {
        let mut keyboard = Keyboard::new();
        keyboard_events(&mut keyboard, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]);
        // A released, B moves to slot 0, C pressed.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0, 0, 0x05, 0x06, 0, 0, 0, 0]),
            [
                InputEvent::KeyUp(KeyCode::A),
                InputEvent::KeyDown(key(0x06))
            ]
        );
    }

    #[test]
    fn rollover_and_faults_change_nothing() {
        let mut keyboard = Keyboard::new();
        keyboard_events(&mut keyboard, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
        for fault in [ROLLOVER, POST_FAIL, UNDEFINED] {
            assert!(
                keyboard_events(
                    &mut keyboard,
                    &[0, 0, fault, fault, fault, fault, fault, fault]
                )
                .is_empty()
            );
        }
        assert_eq!(keyboard.dropped().rollover, 3);
        // A is still down: its release is reported.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0; 8]),
            [InputEvent::KeyUp(KeyCode::A)]
        );
    }

    #[test]
    fn undefined_usages_and_wrong_lengths_are_dropped() {
        let mut keyboard = Keyboard::new();
        // 0xA5 is reserved, 0xE1 is a modifier's usage in a key slot, and
        // A appears twice.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0, 0, 0xA5, 0xE1, 0x04, 0x04, 0, 0]),
            [InputEvent::KeyDown(KeyCode::A)]
        );
        assert_eq!(keyboard.dropped().undefined_usage, 2);
        // Releasing them: only A's release.
        assert_eq!(
            keyboard_events(&mut keyboard, &[0; 8]),
            [InputEvent::KeyUp(KeyCode::A)]
        );
        assert!(keyboard_events(&mut keyboard, &[0; 7]).is_empty());
        assert!(keyboard_events(&mut keyboard, &[0; 9]).is_empty());
        assert_eq!(keyboard.dropped().wrong_length, 2);
        assert_eq!(keyboard.dropped().total(), 4);
    }

    #[test]
    fn unplugging_releases_what_is_down() {
        let mut keyboard = Keyboard::new();
        keyboard_events(&mut keyboard, &[0x01, 0, 0x2C, 0, 0, 0, 0, 0]);
        let mut events = Vec::new();
        keyboard.release_all(|event| events.push(event));
        assert_eq!(
            events,
            [
                InputEvent::KeyUp(KeyCode::LEFT_CTRL),
                InputEvent::KeyUp(KeyCode::SPACE)
            ]
        );
    }

    fn mouse_events(mouse: &mut Mouse, report: &[u8]) -> Vec<InputEvent> {
        let mut events = Vec::new();
        mouse.report(report, |event| events.push(event));
        events
    }

    #[test]
    fn a_mouse_moves_and_clicks() {
        let mut mouse = Mouse::new(InputLimits::new(800, 480).unwrap());
        assert_eq!(
            mouse_events(&mut mouse, &[0, 10, 0xFB]),
            [InputEvent::PointerMove { x: 410, y: 235 }]
        );
        assert_eq!(
            mouse_events(&mut mouse, &[1, 0, 0, 0]),
            [InputEvent::PointerButton {
                button: 0,
                down: true
            }]
        );
        // Moving with the button held, then right down and left up.
        assert_eq!(
            mouse_events(&mut mouse, &[0b11, 0x80, 0x7F]),
            [
                InputEvent::PointerMove { x: 282, y: 362 },
                InputEvent::PointerButton {
                    button: 1,
                    down: true
                }
            ]
        );
        assert_eq!(
            mouse_events(&mut mouse, &[0b10, 0, 0]),
            [InputEvent::PointerButton {
                button: 0,
                down: false
            }]
        );
        // Far off the screen: clamped.
        for _ in 0..10 {
            mouse_events(&mut mouse, &[0b10, 0x7F, 0x7F]);
        }
        assert_eq!(
            mouse_events(&mut mouse, &[0b10, 1, 1]),
            [InputEvent::PointerMove { x: 799, y: 479 }]
        );
        // Vendor bits above the third button are ignored.
        assert!(mouse_events(&mut mouse, &[0b1111_1010, 0, 0]).is_empty());
        let mut events = Vec::new();
        mouse.release_all(|event| events.push(event));
        assert_eq!(
            events,
            [InputEvent::PointerButton {
                button: 1,
                down: false
            }]
        );
    }

    #[test]
    fn short_and_long_mouse_reports_are_dropped() {
        let mut mouse = Mouse::new(InputLimits::new(800, 480).unwrap());
        assert!(mouse_events(&mut mouse, &[1, 2]).is_empty());
        assert!(mouse_events(&mut mouse, &[1; REPORT_MAX + 1]).is_empty());
        assert_eq!(mouse.dropped().wrong_length, 2);
        assert_eq!(mouse_events(&mut mouse, &[0; REPORT_MAX]).len(), 0);
    }

    proptest! {
        #[test]
        fn keyboard_reports_never_panic_and_stay_bounded(
            reports in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..12), 1..20)
        ) {
            let mut keyboard = Keyboard::new();
            let mut down = std::collections::BTreeSet::new();
            for report in reports {
                let mut count = 0;
                keyboard.report(&report, |event| {
                    count += 1;
                    match event {
                        InputEvent::KeyDown(key) => assert!(down.insert(key.usage()), "pressed twice"),
                        InputEvent::KeyUp(key) => assert!(down.remove(&key.usage()), "released while up"),
                        other => panic!("{other:?} from a keyboard"),
                    }
                });
                prop_assert!(count <= MAX_KEYBOARD_EVENTS);
            }
            let mut released = 0;
            keyboard.release_all(|event| {
                if let InputEvent::KeyUp(key) = event {
                    assert!(down.remove(&key.usage()));
                    released += 1;
                }
            });
            prop_assert!(down.is_empty(), "keys left down: {down:?}");
            let _ = released;
        }

        #[test]
        fn mouse_reports_never_panic_and_stay_on_the_screen(
            reports in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..70), 1..20)
        ) {
            let mut mouse = Mouse::new(InputLimits::new(320, 240).unwrap());
            for report in reports {
                let mut count = 0;
                mouse.report(&report, |event| {
                    count += 1;
                    match event {
                        InputEvent::PointerMove { x, y } => assert!((0..320).contains(&x) && (0..240).contains(&y)),
                        InputEvent::PointerButton { button, .. } => assert!(button < 3),
                        other => panic!("{other:?} from a mouse"),
                    }
                });
                prop_assert!(count <= MAX_MOUSE_EVENTS);
            }
        }
    }

    // ---- Key repeat -------------------------------------------

    fn repeats_at(repeat: &mut KeyRepeat, now: u64) -> Vec<InputEvent> {
        let mut out = Vec::new();
        repeat.due(now, |event| out.push(event));
        out
    }

    #[test]
    fn a_held_key_repeats_after_half_a_second_then_thirty_times_a_second() {
        let mut repeat = KeyRepeat::new();
        repeat.saw(InputEvent::KeyDown(KeyCode::DELETE), 1_000);
        assert!(repeats_at(&mut repeat, 1_000 + REPEAT_DELAY_US - 1).is_empty());
        assert_eq!(
            repeats_at(&mut repeat, 1_000 + REPEAT_DELAY_US),
            [InputEvent::KeyDown(KeyCode::DELETE)]
        );
        assert!(repeats_at(&mut repeat, 1_000 + REPEAT_DELAY_US + 10_000).is_empty());
        assert_eq!(
            repeats_at(&mut repeat, 1_000 + REPEAT_DELAY_US + REPEAT_EVERY_US).len(),
            1
        );
        // A second's repeats, a tick every 8 ms: about thirty.
        let start = 1_000 + REPEAT_DELAY_US + REPEAT_EVERY_US;
        let count: usize = (1..=125u64)
            .map(|tick| repeats_at(&mut repeat, start + tick * 8_000).len())
            .sum();
        assert!((29..=31).contains(&count), "{count}");
        repeat.saw(InputEvent::KeyUp(KeyCode::DELETE), start + 1_000_001);
        assert!(repeats_at(&mut repeat, start + 9_000_000).is_empty());
    }

    #[test]
    fn modifiers_and_lock_keys_never_repeat() {
        let shift = KeyCode::new(0xE1).unwrap();
        for key in [shift, KeyCode::CAPS_LOCK, KeyCode::new(0x53).unwrap()] {
            let mut repeat = KeyRepeat::new();
            repeat.saw(InputEvent::KeyDown(key), 0);
            assert!(repeats_at(&mut repeat, 10_000_000).is_empty(), "{key:?}");
        }
        // Shift pressed over a repeating key leaves it repeating.
        let mut repeat = KeyRepeat::new();
        repeat.saw(InputEvent::KeyDown(KeyCode::A), 0);
        repeat.saw(InputEvent::KeyDown(shift), 100_000);
        assert_eq!(repeat.held(), Some(KeyCode::A));
    }

    #[test]
    fn another_key_takes_over_and_the_old_ones_release_changes_nothing() {
        let mut repeat = KeyRepeat::new();
        repeat.saw(InputEvent::KeyDown(KeyCode::A), 0);
        repeat.saw(InputEvent::KeyDown(KeyCode::B), 200_000);
        assert!(
            repeats_at(&mut repeat, REPEAT_DELAY_US).is_empty(),
            "B's own delay"
        );
        repeat.saw(InputEvent::KeyUp(KeyCode::A), 300_000);
        assert_eq!(
            repeats_at(&mut repeat, 200_000 + REPEAT_DELAY_US),
            [InputEvent::KeyDown(KeyCode::B)]
        );
    }

    #[test]
    fn a_late_tick_catches_up_by_a_few_never_a_flood_and_stop_stops() {
        let mut repeat = KeyRepeat::new();
        repeat.saw(InputEvent::KeyDown(KeyCode::Z), 0);
        let late = REPEAT_DELAY_US + 60 * REPEAT_EVERY_US;
        assert_eq!(
            repeats_at(&mut repeat, late).len(),
            usize::try_from(MAX_REPEATS_AT_ONCE).unwrap()
        );
        assert!(repeats_at(&mut repeat, late).is_empty(), "then from now on");
        assert_eq!(repeats_at(&mut repeat, late + REPEAT_EVERY_US).len(), 1);
        repeat.stop();
        assert!(repeats_at(&mut repeat, late + 10 * REPEAT_EVERY_US).is_empty());
    }

    fn key_event() -> impl Strategy<Value = (bool, u8)> {
        (
            any::<bool>(),
            prop::sample::select(vec![0x04u8, 0x05, 0x06, 0x39, 0xE1]),
        )
    }

    proptest! {
        /// Whatever is pressed and released, and however the ticks fall:
        /// only the last key pressed that repeats and is still down ever
        /// repeats, a few at most a call, and none before its delay.
        #[test]
        fn only_the_last_key_still_down_repeats(
            steps in prop::collection::vec((key_event(), 0u64..200_000), 0..60),
        ) {
            let mut repeat = KeyRepeat::new();
            let mut now = 0u64;
            let mut down: Option<(u8, u64)> = None;
            for ((press, usage), step) in steps {
                now += step;
                let key = KeyCode::new(u16::from(usage)).unwrap();
                if press {
                    repeat.saw(InputEvent::KeyDown(key), now);
                    if !key.is_modifier() && usage != 0x39 {
                        down = Some((usage, now));
                    }
                } else {
                    repeat.saw(InputEvent::KeyUp(key), now);
                    if down.is_some_and(|(held, _)| held == usage) {
                        down = None;
                    }
                }
                let out = repeats_at(&mut repeat, now);
                prop_assert!(out.len() <= usize::try_from(MAX_REPEATS_AT_ONCE).unwrap());
                for event in out {
                    let Some((held, since)) = down else {
                        panic!("a repeat with no key down: {event:?}");
                    };
                    prop_assert_eq!(event, InputEvent::KeyDown(KeyCode::new(u16::from(held)).unwrap()));
                    prop_assert!(now >= since + REPEAT_DELAY_US);
                }
            }
        }
    }
}
