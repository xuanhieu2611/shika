//! The app side of the Lead's `shika` command (see `docs/lead-agent.md`).
//!
//! A background thread accepts connections on a Unix socket. Each connection
//! carries one request. The thread hands it to the UI thread over a channel
//! that `Shika::tick` drains, the same way notification clicks arrive, and
//! then blocks on a reply channel, holding no lock. All state lives on the UI
//! thread: the Lead is the card that holds the request's token, so the token
//! dies with the card and needs no table.
//!
//! `wait` is a [`Waiter`] on the Lead's card. The tick resolves it from the
//! cards' current status, so there is no polling thread. The doorbell
//! ([`Doorbell`]) is resolved from the same tick: when workers settle and the
//! Lead is idle and its author is not typing, Shika submits one `[shika]` line
//! into the Lead's terminal. `send` and `key` use the same typing path
//! ([`Shika::type_into`]) with the guard in [`input_refusal`].
mod activity;
mod close;
mod commands;
mod doorbell;
mod input;
mod launch;
mod publish;
mod server;
mod tasks;
mod wait;

pub(crate) use close::*;
pub(crate) use commands::*;
pub(crate) use doorbell::*;
pub(crate) use input::*;
pub(crate) use launch::*;
pub(crate) use publish::*;
pub(crate) use server::*;
pub(crate) use tasks::*;
pub(crate) use wait::*;

use crate::Shika;
use crate::lead_steps::SharedSteps;
use gpui::{Context, Window};
use std::time::Instant;

/// Everything the Lead's card keeps for the control socket.
pub struct LeadState {
    pub token: String,
    ledger: Ledger,
    waiters: Vec<Waiter>,
    unconfirmed: Vec<Unconfirmed>,
    doorbell: Doorbell,
    /// The activity steps of the running turn (see `lead_steps`).
    pub steps: SharedSteps,
    /// The step list version last pushed to the chat and the card.
    steps_seen: u64,
}

impl LeadState {
    pub fn new(token: String) -> Self {
        Self {
            token,
            ledger: Ledger::default(),
            waiters: Vec::new(),
            unconfirmed: Vec::new(),
            doorbell: Doorbell::default(),
            steps: SharedSteps::default(),
            steps_seen: 0,
        }
    }
}

impl Shika {
    /// Called every tick: answers queued commands, then pending waits.
    pub(crate) fn drain_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(incoming) = self.control.as_ref().and_then(Server::next) {
            self.handle_control(incoming, window, cx);
        }
        self.resume_deferred_close(window, cx);
        self.settle_lead_dialog(window, cx);
        let now = Instant::now();
        self.resolve_waiters(now);
        self.ring_doorbells(now, window, cx);
        self.deliver_restart_summaries(now, window, cx);
    }

    /// The Lead card for `project`, whether or not it is running yet.
    pub(crate) fn lead_card(&self, project: &str) -> Option<usize> {
        self.cards
            .iter()
            .position(|card| card.lead.is_some() && card.project == project)
    }
}

#[cfg(test)]
mod tests;
