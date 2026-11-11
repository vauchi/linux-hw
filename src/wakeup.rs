// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The one-shot wakeup Core asks for with `ScheduleWakeup` (ADR-044 Am2a,
//! ADR-077). Core computes the delay (vauchi/private#548); a new schedule
//! replaces a pending one, so a live QR exchange's sub-second heartbeat
//! never fires twice.

use std::time::Duration;
use tokio::time::{Instant, sleep_until};
use vauchi_contract::Event;

#[derive(Debug, Default)]
pub(crate) struct WakeupTimer {
    due: Option<Instant>,
}

impl WakeupTimer {
    pub(crate) fn schedule(&mut self, delay_millis: u32) {
        self.due = Some(Instant::now() + Duration::from_millis(u64::from(delay_millis)));
    }

    /// `WakeupFired` if the pending wakeup is due, without waiting.
    pub(crate) fn pop(&mut self) -> Option<Event> {
        match self.due {
            Some(due) if due <= Instant::now() => {
                self.due = None;
                Some(Event::WakeupFired)
            }
            _ => None,
        }
    }

    /// Waits for the pending wakeup; `None` at once when none is pending.
    pub(crate) async fn next_event(&mut self) -> Option<Event> {
        let due = self.due?;
        sleep_until(due).await;
        self.due = None;
        Some(Event::WakeupFired)
    }
}
