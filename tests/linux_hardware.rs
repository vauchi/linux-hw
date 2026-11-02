// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `LinuxHardware` is what an app holds: linux-hw's own runtime, the
//! sessions on it and the capability report, behind plain blocking calls,
//! so GTK, CLI and TUI need no runtime of their own (ADR-077 item 5,
//! plan Phase 2 "linux-hw owns its runtime and event loop").
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::mpsc;
use vauchi_contract::exchange_types::{DeviceCapabilities, Platform};
use vauchi_contract::{
    BleAdvertisementObservation, BleLinkDirection, BleServiceUuid, Command, Event,
};
use vauchi_linux_hw::{
    Advertising, Backends, Discovery, EventSender, ExecuteError, HardwareFailure, HostDevices,
    LinuxHardware, Outbound,
};

/// The longest attribute value ATT carries, every byte at its widest in JSON.
fn largest_write() -> Event {
    Event::BleCharacteristicNotified {
        device_id: "AA:BB:CC:DD:EE:FF".into(),
        direction: BleLinkDirection::Inbound,
        uuid: "a1b2c3d4-e5f6-7890-abcd-ef1234567892".into(),
        data: vec![255; 512],
    }
}

struct CentralWritesTheLargestValue;

impl Advertising for CentralWritesTheLargestValue {
    fn advertise(
        &self,
        _: String,
        _: Vec<u8>,
        _: Outbound,
        mut events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(async move {
            events.send(largest_write()).await.unwrap();
            std::future::pending().await
        })
    }
}

// @internal
#[test]
fn a_central_writing_the_largest_attribute_value_reaches_core_whole() {
    let backends = Backends {
        advertising: Some(Arc::new(CentralWritesTheLargestValue)),
        ..Backends::default()
    };
    let mut hardware = LinuxHardware::with_backends(backends, bluetooth_only()).unwrap();

    hardware
        .execute(Command::BleStartAdvertising {
            service_uuid: "core-service".into(),
            payload: vec![1, 2],
        })
        .unwrap();

    assert_eq!(next_event(&mut hardware), largest_write());
}

struct Released(mpsc::Sender<&'static str>);

impl Drop for Released {
    fn drop(&mut self) {
        let _ = self.0.send("released");
    }
}

struct OnePeer {
    lifecycle: mpsc::Sender<&'static str>,
}

impl Discovery for OnePeer {
    fn scan(
        &self,
        _: String,
        mut events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(async move {
            let _released = Released(self.lifecycle.clone());
            let observation = BleAdvertisementObservation::new(
                "peer".into(),
                -40,
                vec![BleServiceUuid::Uuid16([0xAB, 0xCD])],
            )
            .unwrap();
            events
                .send(Event::BleAdvertisementObserved(observation))
                .await
                .unwrap();
            std::future::pending().await
        })
    }
}

fn bluetooth_only() -> HostDevices {
    HostDevices {
        bluetooth_adapter: true,
        ..HostDevices::default()
    }
}

fn with_scanner() -> (LinuxHardware, mpsc::Receiver<&'static str>) {
    let (lifecycle, released) = mpsc::channel();
    let backends = Backends {
        discovery: Some(Arc::new(OnePeer { lifecycle })),
        ..Backends::default()
    };
    (
        LinuxHardware::with_backends(backends, bluetooth_only()).unwrap(),
        released,
    )
}

fn next_event(hardware: &mut LinuxHardware) -> Event {
    loop {
        if let Some(event) = hardware.pop() {
            return event;
        }
        std::thread::yield_now();
    }
}

// @scenario: contact_exchange.feature :: Discover nearby Vauchi users via BLE
#[test]
fn a_scan_runs_on_linux_hws_runtime_and_the_caller_needs_none() {
    let (mut hardware, _released) = with_scanner();

    hardware
        .execute(Command::BleStartScanning {
            service_uuid: "core-service".into(),
        })
        .unwrap();

    match next_event(&mut hardware) {
        Event::BleAdvertisementObserved(observation) => assert_eq!(observation.id(), "peer"),
        other => panic!("expected the scan's observation, got {other:?}"),
    }
}

// @internal
#[test]
fn dropping_the_hardware_releases_the_running_scan() {
    let (mut hardware, released) = with_scanner();
    hardware
        .execute(Command::BleStartScanning {
            service_uuid: "core-service".into(),
        })
        .unwrap();
    next_event(&mut hardware);

    drop(hardware);

    assert_eq!(released.recv().unwrap(), "released");
}

// @internal
#[test]
fn the_report_names_what_the_devices_allow() {
    let (hardware, _released) = with_scanner();

    assert_eq!(
        hardware.report(),
        Event::HardwareCapabilitiesReported {
            capabilities: DeviceCapabilities {
                has_ble: cfg!(target_os = "linux"),
                has_internet: true,
                platform: Platform::Desktop,
                ..DeviceCapabilities::default()
            }
        }
    );
}

// @internal
#[test]
fn a_command_without_a_backend_is_unsupported() {
    let (mut hardware, _released) = with_scanner();

    assert_eq!(
        hardware.execute(Command::BleStartAdvertising {
            service_uuid: "core-service".into(),
            payload: vec![1],
        }),
        Err(ExecuteError::Unsupported)
    );
    assert_eq!(
        hardware.execute(Command::AudioStop),
        Err(ExecuteError::Unsupported)
    );
}
