// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use vauchi_contract::executor::InboxLimits;
use vauchi_contract::{
    BleAdvertisementObservation, BleLinkDirection, BleServiceUuid, Command, Event,
};
use vauchi_linux_hw::{
    Advertising, BleAdvertiser, BleScanner, Discovery, EventSender, ExecuteError, HardwareFailure,
    HardwareShell, Notification, Outbound,
};

struct Released(mpsc::UnboundedSender<String>);

impl Drop for Released {
    fn drop(&mut self) {
        let _ = self.0.send("released".into());
    }
}

struct RecordingDiscovery {
    lifecycle: mpsc::UnboundedSender<String>,
    observed: mpsc::UnboundedSender<()>,
}

impl Discovery for RecordingDiscovery {
    fn scan(
        &self,
        service_uuid: String,
        mut events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(async move {
            let _released = Released(self.lifecycle.clone());
            self.lifecycle.send(format!("scan {service_uuid}")).unwrap();
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
            self.observed.send(()).unwrap();
            std::future::pending().await
        })
    }
}

struct RecordingAdvertising {
    lifecycle: mpsc::UnboundedSender<String>,
    notified: mpsc::UnboundedSender<Notification>,
}

impl Advertising for RecordingAdvertising {
    fn advertise(
        &self,
        service_uuid: String,
        payload: Vec<u8>,
        mut outbound: Outbound,
        _events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(async move {
            let _released = Released(self.lifecycle.clone());
            self.lifecycle
                .send(format!("advertise {service_uuid} {payload:?}"))
                .unwrap();
            while let Some(notification) = outbound.next().await {
                self.notified.send(notification).unwrap();
            }
            std::future::pending().await
        })
    }
}

fn limits() -> InboxLimits {
    InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(1).unwrap(),
        event_bytes: NonZeroUsize::new(512).unwrap(),
    }
}

struct Fixture {
    shell: HardwareShell,
    scans: mpsc::UnboundedReceiver<String>,
    observed: mpsc::UnboundedReceiver<()>,
    adverts: mpsc::UnboundedReceiver<String>,
    notified: mpsc::UnboundedReceiver<Notification>,
}

fn fixture() -> Fixture {
    let runtime = tokio::runtime::Handle::current();
    let (scan_tx, scans) = mpsc::unbounded_channel();
    let (observed_tx, observed) = mpsc::unbounded_channel();
    let (advert_tx, adverts) = mpsc::unbounded_channel();
    let (notified_tx, notified) = mpsc::unbounded_channel();
    let scanner = BleScanner::with_backend(
        Arc::new(RecordingDiscovery {
            lifecycle: scan_tx,
            observed: observed_tx,
        }),
        runtime.clone(),
        limits(),
    )
    .unwrap();
    let advertiser = BleAdvertiser::with_backend(
        Arc::new(RecordingAdvertising {
            lifecycle: advert_tx,
            notified: notified_tx,
        }),
        runtime,
        limits(),
    )
    .unwrap();
    Fixture {
        shell: HardwareShell::new()
            .with_scanner(scanner)
            .with_advertiser(advertiser),
        scans,
        observed,
        adverts,
        notified,
    }
}

// @scenario: contact_exchange.feature :: Discover nearby Vauchi users via BLE
#[tokio::test]
async fn a_scan_command_runs_the_scan_and_its_observation_reaches_pop() {
    let mut f = fixture();

    f.shell
        .execute(Command::BleStartScanning {
            service_uuid: "core-service".into(),
        })
        .unwrap();

    assert_eq!(f.scans.recv().await.unwrap(), "scan core-service");
    f.observed.recv().await.unwrap();
    match f.shell.pop() {
        Some(Event::BleAdvertisementObserved(observation)) => {
            assert_eq!(observation.id(), "peer");
        }
        other => panic!("expected the scan's observation, got {other:?}"),
    }
    assert_eq!(f.shell.pop(), None);

    f.shell.execute(Command::BleStopScanning).unwrap();
    assert_eq!(f.scans.recv().await.unwrap(), "released");
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn advertising_runs_until_core_sends_the_stop() {
    let mut f = fixture();

    f.shell
        .execute(Command::BleStartAdvertising {
            service_uuid: "core-service".into(),
            payload: vec![7, 9],
        })
        .unwrap();
    assert_eq!(
        f.adverts.recv().await.unwrap(),
        "advertise core-service [7, 9]"
    );

    f.shell
        .execute(Command::BleWriteCharacteristic {
            device_id: "central".into(),
            direction: BleLinkDirection::Inbound,
            uuid: "char".into(),
            data: vec![1, 2, 3],
        })
        .unwrap();
    assert_eq!(
        f.notified.recv().await.unwrap(),
        Notification {
            device_id: "central".into(),
            uuid: "char".into(),
            data: vec![1, 2, 3],
        }
    );

    f.shell.execute(Command::BleStopAdvertising).unwrap();
    assert_eq!(f.adverts.recv().await.unwrap(), "released");
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_command_without_a_port_is_unsupported_and_reports_nothing() {
    let mut f = fixture();
    let outbound_write = Command::BleWriteCharacteristic {
        device_id: "peripheral".into(),
        direction: BleLinkDirection::Outbound,
        uuid: "char".into(),
        data: vec![1],
    };

    for command in [
        Command::AccelerometerStop,
        Command::BleConnect {
            device_id: "peer".into(),
        },
        outbound_write,
    ] {
        let name = format!("{command:?}");
        assert_eq!(
            f.shell.execute(command),
            Err(ExecuteError::Unsupported),
            "{name}"
        );
    }
    assert_eq!(f.shell.pop(), None);
    f.shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_shell_without_an_executor_for_the_command_reports_it_unsupported() {
    let mut shell = HardwareShell::new();

    assert_eq!(
        shell.execute(Command::BleStopAdvertising),
        Err(ExecuteError::Unsupported)
    );
    assert_eq!(
        shell.execute(Command::BleStartScanning {
            service_uuid: "core-service".into(),
        }),
        Err(ExecuteError::Unsupported)
    );
    shell.shutdown().await.unwrap();
}
