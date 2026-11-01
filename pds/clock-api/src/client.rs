// SPDX-License-Identifier: MIT OR Apache-2.0
//! The call itself, from inside a PD (the `client` feature).

use sel4_microkit::{Channel, MessageInfo, with_msg_regs, with_msg_regs_mut};

use crate::{ClockError, Message, Reading, Request};

/// A reader's end of its channel to the clock PD. The system description
/// gives that end `pp="true"`; the channel number is the reader's `id`.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    channel: Channel,
}

impl Client {
    /// The reader's end of channel `channel`.
    #[must_use]
    pub const fn new(channel: Channel) -> Self {
        Self { channel }
    }

    /// The channel. A source also notifies the clock PD on it when samples
    /// wait on its ring.
    #[must_use]
    pub const fn channel(&self) -> Channel {
        self.channel
    }

    /// The time now, as the clock knows it.
    ///
    /// # Errors
    ///
    /// As [`Message::read_reply`].
    pub fn read(&self) -> Result<Reading, ClockError> {
        self.call(&Message::request_for(Request::Read)).read_reply()
    }

    /// Sends `request` as it is, and returns the reply: for tests of
    /// malformed requests, and of a sample offered by call.
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
