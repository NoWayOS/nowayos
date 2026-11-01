// SPDX-License-Identifier: MIT OR Apache-2.0
//! The calls themselves, from inside a PD (the `client` feature).

use sel4_microkit::{Channel, MessageInfo, with_msg_regs, with_msg_regs_mut};

use crate::{Instant, Message, Request, TimerError};

/// A client's end of its channel to the timer PD. The system description
/// gives that end `pp="true"`; the channel number is the client's `id`, and
/// the timer PD's notifications that a timeout expired arrive on the same
/// channel.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    channel: Channel,
}

impl Client {
    /// The client end of channel `channel`.
    #[must_use]
    pub const fn new(channel: Channel) -> Self {
        Self { channel }
    }

    /// The channel, to recognise the timer PD's notifications.
    #[must_use]
    pub const fn channel(&self) -> Channel {
        self.channel
    }

    /// The time now.
    ///
    /// # Errors
    ///
    /// As [`Message::now_reply`].
    pub fn now(&self) -> Result<Instant, TimerError> {
        self.call(&Message::request_for(Request::Now)).now_reply()
    }

    /// Asks to be notified `nanos` nanoseconds from now, under `id`.
    ///
    /// # Errors
    ///
    /// As [`Message::set_reply`]: a zero or too long timeout, an id already
    /// set, or too many timeouts.
    pub fn set_timeout(&self, id: u64, nanos: u64) -> Result<(), TimerError> {
        self.call(&Message::request_for(Request::SetTimeout { id, nanos }))
            .set_reply()
    }

    /// Forgets timeout `id`; whether it was still there.
    ///
    /// # Errors
    ///
    /// As [`Message::cancel_reply`].
    pub fn cancel(&self, id: u64) -> Result<bool, TimerError> {
        self.call(&Message::request_for(Request::Cancel { id }))
            .cancel_reply()
    }

    /// The earliest of this client's expired timeouts not yet collected.
    ///
    /// # Errors
    ///
    /// As [`Message::expired_reply`].
    pub fn expired(&self) -> Result<Option<u64>, TimerError> {
        self.call(&Message::request_for(Request::Expired))
            .expired_reply()
    }

    /// Sends `request` as it is, and returns the reply: for tests of
    /// malformed requests.
    #[must_use]
    pub fn call(&self, request: &Message) -> Message {
        let words = request.words().unwrap_or_default();
        with_msg_regs_mut(|regs| {
            for (reg, word) in regs.iter_mut().zip(words) {
                *reg = *word;
            }
        });
        let reply = self
            .channel
            .pp_call(MessageInfo::new(request.label(), words.len()));
        let count = reply.count();
        with_msg_regs(|regs| {
            Message::received(reply.label(), count, |index| {
                regs.get(index).copied().unwrap_or_default()
            })
        })
    }
}
