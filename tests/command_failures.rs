// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use vauchi_contract::executor::DeliveryError;
use vauchi_contract::{BleLinkDirection, Command, Event};
use vauchi_linux_hw::{
    Backends, Central, EventSender, ExecuteError, HardwareFailure, HostDevices, LinkRequests,
    LinuxHardware,
};

struct BusyCentral;

impl Central for BusyCentral {
    fn connect(
        &self,
        _: String,
        requests: LinkRequests,
        _: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(async move {
            let _requests = requests;
            std::future::pending().await
        })
    }
}

fn connected() -> LinuxHardware {
    let backends = Backends {
        central: Some(Arc::new(BusyCentral)),
        ..Backends::default()
    };
    let mut hardware = LinuxHardware::with_backends(backends, HostDevices::default()).unwrap();
    hardware
        .execute(Command::BleConnect {
            device_id: "peer".into(),
        })
        .unwrap();
    hardware
}

fn write(bytes: usize) -> Command {
    Command::BleWriteCharacteristic {
        device_id: "peer".into(),
        direction: BleLinkDirection::Outbound,
        uuid: "attribute".into(),
        data: vec![0xAB; bytes],
    }
}

// @internal — the host may ignore execute's result; Core must still hear the failure.
#[test]
fn a_full_command_queue_reports_a_failure_to_core() {
    let mut hardware = connected();
    for _ in 0..64 {
        hardware.execute(write(1)).unwrap();
    }

    assert_eq!(
        hardware.execute(write(1)),
        Err(ExecuteError::Delivery(DeliveryError::Full))
    );
    assert_eq!(
        hardware.pop(),
        Some(Event::HardwareError {
            transport: "BLE".into(),
            error: "hardware command queue is full".into(),
        })
    );
    assert_eq!(hardware.pop(), None);
}
