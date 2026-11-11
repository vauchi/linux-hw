// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ScheduleWakeup` is a hardware command (ADR-077); linux-hw reports
//! `WakeupFired` once Core's delay has passed, for every Linux pairing
//! (private#1029). Until now only gtk-display ran it, so the Qt app never
//! woke Core.

use std::time::Duration;
use tokio::time::{advance, timeout};
use vauchi_contract::{Command, Event};
use vauchi_linux_hw::HardwareShell;

fn wakeup_in_millis(millis: u32) -> Command {
    Command::schedule_wakeup(0, 60, 0, Some(millis))
}

async fn next_within(shell: &mut HardwareShell, millis: u64) -> Option<Event> {
    timeout(Duration::from_millis(millis), shell.next_event())
        .await
        .ok()
        .flatten()
}

#[tokio::test(start_paused = true)]
async fn a_wakeup_fires_once_its_delay_has_passed_and_not_before() {
    let mut shell = HardwareShell::new();
    shell
        .execute(wakeup_in_millis(250))
        .expect("linux-hw runs ScheduleWakeup");

    assert_eq!(next_within(&mut shell, 249).await, None);
    assert_eq!(next_within(&mut shell, 1).await, Some(Event::WakeupFired));
    advance(Duration::from_secs(10)).await;
    assert_eq!(shell.pop(), None, "one schedule fires once");
}

#[tokio::test(start_paused = true)]
async fn a_new_wakeup_replaces_the_pending_one() {
    let mut shell = HardwareShell::new();
    shell
        .execute(wakeup_in_millis(1_000))
        .expect("first schedule");
    shell.execute(wakeup_in_millis(100)).expect("replacement");

    assert_eq!(next_within(&mut shell, 100).await, Some(Event::WakeupFired));
    assert_eq!(
        next_within(&mut shell, 5_000).await,
        None,
        "the replaced 1 s wakeup does not fire as well"
    );
}

#[tokio::test(start_paused = true)]
async fn a_due_wakeup_is_reported_by_pop_without_waiting() {
    let mut shell = HardwareShell::new();
    shell
        .execute(wakeup_in_millis(0))
        .expect("immediate schedule");

    assert_eq!(shell.pop(), Some(Event::WakeupFired));
    assert_eq!(shell.pop(), None);
}
