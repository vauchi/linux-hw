// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::Duration;
use vauchi_contract::{Command, Event};
use vauchi_linux_hw::{Backends, ExecuteError, HostDevices, LinuxHardware, StartError};

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

// @internal — reproduce persistent native I/O failure in an isolated child;
// the old empty-backend fallback fails here too, whereas timers need no I/O.
#[cfg(unix)]
#[test]
fn timer_only_fallback_survives_exhausted_file_descriptors() {
    const CHILD: &str = "VAUCHI_TEST_HARDWARE_FD_EXHAUSTION";
    if std::env::var_os(CHILD).is_some() {
        let mut held_files = Vec::new();
        let exhausted = loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => held_files.push(file),
                Err(error) => break error,
            }
        };
        let native = LinuxHardware::with_backends(Backends::default(), HostDevices::default());
        match native {
            Err(StartError::Runtime(error)) => {
                assert_eq!(error.raw_os_error(), exhausted.raw_os_error());
            }
            Err(error) => panic!("wrong native failure: {error}"),
            Ok(_) => panic!("native I/O runtime started with no file descriptors"),
        }
        hardware_without_devices_reports_unavailable_ports_and_keeps_wakeups();
        return;
    }

    let output = std::process::Command::new("sh")
        .args([
            "-ec",
            "ulimit -n 64; exec \"$1\" --exact timer_only_fallback_survives_exhausted_file_descriptors --nocapture",
            "hardware-fallback-test",
        ])
        .arg(std::env::current_exe().unwrap())
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated fallback failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
