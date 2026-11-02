// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `BlueZCentral` against a scripted remote GATT server on a private bus:
//! the D-Bus conversation is real, the radio is not (private#770).

#![cfg(target_os = "linux")]

use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tempfile::TempDir;
use vauchi_contract::executor::InboxLimits;
use vauchi_contract::{BleLinkDirection, Event};
use vauchi_linux_hw::fake_bluez::{Call, FakeBlueZ, REMOTE_MTU, RemoteCharacteristic, Script};
use vauchi_linux_hw::{BleCentral, BlueZCentral, LinkRequest};

struct Bus {
    bluez: FakeBlueZ,
    _directory: TempDir,
}

impl Deref for Bus {
    type Target = FakeBlueZ;
    fn deref(&self) -> &FakeBlueZ {
        &self.bluez
    }
}

impl DerefMut for Bus {
    fn deref_mut(&mut self) -> &mut FakeBlueZ {
        &mut self.bluez
    }
}

const PEER: &str = "AA:BB:CC:DD:EE:03";
const EXCHANGE_PAYLOAD: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567891";
const CARD_EXCHANGE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567892";
const HANDSHAKE_WRITE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567894";
const PAYLOAD: [u8; 3] = [7, 8, 9];

async fn start(script: Script) -> Bus {
    let directory = tempfile::tempdir().unwrap();
    let bluez = FakeBlueZ::start(script, directory.path()).await;
    bluez.remote(
        PEER,
        vec![
            RemoteCharacteristic::new(EXCHANGE_PAYLOAD, &["read", "notify"], &PAYLOAD),
            RemoteCharacteristic::new(CARD_EXCHANGE, &["write", "notify"], &[]),
            RemoteCharacteristic::new(HANDSHAKE_WRITE, &["write"], &[]),
        ],
    );
    Bus {
        bluez,
        _directory: directory,
    }
}

fn central(bluez: &FakeBlueZ) -> BleCentral {
    BleCentral::with_backend(
        Arc::new(BlueZCentral::at(bluez.socket())),
        tokio::runtime::Handle::current(),
        InboxLimits {
            operations: NonZeroUsize::new(1).unwrap(),
            events: NonZeroUsize::new(8).unwrap(),
            event_bytes: NonZeroUsize::new(4096).unwrap(),
        },
    )
    .unwrap()
}

async fn next_event(central: &mut BleCentral) -> Event {
    central
        .next_event()
        .await
        .expect("the link reports an event")
}

/// Connect and take the two events that open the link.
async fn connected(bluez: &mut Bus) -> BleCentral {
    let mut central = central(bluez);
    central.connect(PEER.into()).unwrap();
    assert_eq!(
        next_event(&mut central).await,
        Event::BleMtuNegotiated {
            device_id: PEER.into(),
            mtu: u32::from(REMOTE_MTU),
        }
    );
    assert_eq!(
        next_event(&mut central).await,
        Event::BleConnected {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
        }
    );
    central
}

async fn subscriptions(bluez: &mut Bus) -> Vec<Call> {
    let mut calls = vec![
        bluez.calls.recv().await.unwrap(),
        bluez.calls.recv().await.unwrap(),
    ];
    calls.sort_by_key(|call| format!("{call:?}"));
    calls
}

// @scenario: contact_exchange.feature :: Discover nearby Vauchi users via BLE
#[tokio::test]
async fn connecting_subscribes_to_every_notifying_characteristic_then_reports_the_link() {
    let mut bluez = start(Script::default()).await;
    let mut central = connected(&mut bluez).await;

    assert_eq!(
        bluez.calls.recv().await.unwrap(),
        Call::Connect {
            address: PEER.into()
        }
    );
    assert_eq!(
        subscriptions(&mut bluez).await,
        vec![
            Call::StartNotify {
                uuid: EXCHANGE_PAYLOAD.into()
            },
            Call::StartNotify {
                uuid: CARD_EXCHANGE.into()
            },
        ]
    );
    central.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn cores_writes_and_reads_reach_the_remote_and_its_answer_comes_back() {
    let mut bluez = start(Script::default()).await;
    let mut central = connected(&mut bluez).await;
    bluez.calls.recv().await.unwrap();
    subscriptions(&mut bluez).await;

    central
        .request(
            PEER,
            LinkRequest::Write {
                uuid: HANDSHAKE_WRITE.into(),
                data: vec![1, 2],
            },
        )
        .unwrap();
    central
        .request(
            PEER,
            LinkRequest::Read {
                uuid: EXCHANGE_PAYLOAD.into(),
            },
        )
        .unwrap();

    assert_eq!(
        bluez.calls.recv().await.unwrap(),
        Call::WriteValue {
            uuid: HANDSHAKE_WRITE.into(),
            data: vec![1, 2]
        }
    );
    assert_eq!(
        next_event(&mut central).await,
        Event::BleCharacteristicRead {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: EXCHANGE_PAYLOAD.into(),
            data: PAYLOAD.to_vec(),
        }
    );
    central.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_remote_notification_reaches_core() {
    let mut bluez = start(Script::default()).await;
    let mut central = connected(&mut bluez).await;

    bluez.remote_notifies(PEER, CARD_EXCHANGE, &[4, 5]).await;

    assert_eq!(
        next_event(&mut central).await,
        Event::BleCharacteristicNotified {
            device_id: PEER.into(),
            direction: BleLinkDirection::Outbound,
            uuid: CARD_EXCHANGE.into(),
            data: vec![4, 5],
        }
    );
    central.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_remote_that_drops_the_link_is_reported_disconnected() {
    let mut bluez = start(Script::default()).await;
    let mut central = connected(&mut bluez).await;

    bluez.remote_drops(PEER).await;

    assert!(matches!(
        next_event(&mut central).await,
        Event::BleDisconnected { device_id, direction: BleLinkDirection::Outbound, .. }
            if device_id == PEER
    ));
    central.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn cores_disconnect_disconnects_the_device() {
    let mut bluez = start(Script::default()).await;
    let mut central = connected(&mut bluez).await;
    bluez.calls.recv().await.unwrap();
    subscriptions(&mut bluez).await;

    central.disconnect(PEER).unwrap();

    assert_eq!(
        bluez.calls.recv().await.unwrap(),
        Call::Disconnect {
            address: PEER.into()
        }
    );
    central.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_refused_connection_is_a_link_failure() {
    let mut bluez = start(Script {
        connect_error: Some("org.bluez.Error.Failed"),
        ..Script::default()
    })
    .await;
    let mut central = central(&bluez);

    central.connect(PEER.into()).unwrap();

    assert_eq!(
        next_event(&mut central).await,
        Event::HardwareError {
            transport: "BLE".into(),
            error: "Bluetooth link failed".into(),
        }
    );
    assert_eq!(
        bluez.calls.recv().await.unwrap(),
        Call::Connect {
            address: PEER.into()
        }
    );
    central.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_device_id_that_is_not_an_address_fails_before_touching_bluez() {
    let mut bluez = start(Script::default()).await;
    for device_id in [
        "",
        "AA:BB:CC:DD:EE",
        "../../org/bluez",
        "AA:BB:CC:DD:EE:GG",
        "AA_BB_CC_DD_EE_03",
    ] {
        let mut central = central(&bluez);

        central.connect(device_id.into()).unwrap();

        assert_eq!(
            next_event(&mut central).await,
            Event::HardwareError {
                transport: "BLE".into(),
                error: "Bluetooth link failed".into(),
            },
            "{device_id:?}"
        );
        central.shutdown().await.unwrap();
    }
    assert!(bluez.calls.try_recv().is_err(), "no call reached BlueZ");
}

// @internal
#[tokio::test]
async fn without_a_powered_adapter_bluetooth_is_unavailable() {
    let bluez = start(Script {
        adapter_powered: Some(false),
        ..Script::default()
    })
    .await;
    let mut central = central(&bluez);

    central.connect(PEER.into()).unwrap();

    assert_eq!(
        next_event(&mut central).await,
        Event::HardwareUnavailable {
            transport: "BLE".into()
        }
    );
    central.shutdown().await.unwrap();
}
