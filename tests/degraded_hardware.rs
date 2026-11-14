// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::Duration;
use vauchi_contract::{Command, Event};
use vauchi_linux_hw::{ExecuteError, HostDevices, LinuxHardware};

// @internal — a host can still render and wake Core when native hardware startup fails.
#[test]
fn hardware_without_devices_reports_unavailable_ports_and_keeps_wakeups() {
    let mut hardware = LinuxHardware::without_devices();
    assert_eq!(hardware.report(), HostDevices::default().report());
    assert_eq!(
        hardware.execute(Command::QrRequestScan),
        Err(ExecuteError::Unsupported)
    );
    assert_eq!(
        hardware.execute(Command::schedule_wakeup(0, 60, 0, Some(0))),
        Ok(())
    );
    assert_eq!(
        hardware.wait_for_event(Duration::from_secs(1)),
        Some(Event::WakeupFired)
    );
    assert_eq!(hardware.pop(), None);
    assert_eq!(hardware.wait_for_event(Duration::ZERO), None);
}
