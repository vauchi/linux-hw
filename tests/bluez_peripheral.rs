// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(target_os = "linux")]

use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tempfile::TempDir;
use vauchi_contract::executor::{DeliveryError, InboxLimits};
use vauchi_contract::{BleLinkDirection, Event};
use vauchi_linux_hw::fake_bluez::{Call, FakeBlueZ, Script};
use vauchi_linux_hw::{BleAdvertiser, BlueZAdvertising, Notification};

/// The fake with the directory its socket lives in; the fields drop in
/// order, so the bus stops before its directory is removed.
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

async fn start(script: Script) -> Bus {
    let directory = tempfile::tempdir().unwrap();
    let bluez = FakeBlueZ::start(script, directory.path()).await;
    Bus {
        bluez,
        _directory: directory,
    }
}

const SERVICE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
const EXCHANGE_PAYLOAD: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567891";
const CARD_EXCHANGE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567892";
const CHALLENGE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567893";
const HANDSHAKE_WRITE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567894";
const HANDSHAKE_NOTIFY: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567895";
const DATA_WRITE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567896";
const DATA_NOTIFY: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567897";
const PAYLOAD: [u8; 4] = [0xab, 0xcd, 0x01, 0x02];
/// The payload's first two bytes as a 16-bit Bluetooth-base service UUID,
/// the token the phones advertise and Core decodes.
const TOKEN: &str = "0000abcd-0000-1000-8000-00805f9b34fb";
const CENTRAL: &str = "AA:BB:CC:DD:EE:02";

/// Room for one full attribute value (512 bytes) as an event.
const EVENT_BYTES: usize = 4096;

fn advertiser(bluez: &FakeBlueZ) -> BleAdvertiser {
    advertiser_with(bluez, EVENT_BYTES)
}

fn advertiser_with(bluez: &FakeBlueZ, event_bytes: usize) -> BleAdvertiser {
    BleAdvertiser::with_backend(
        Arc::new(BlueZAdvertising::at(bluez.socket())),
        tokio::runtime::Handle::current(),
        InboxLimits {
            operations: NonZeroUsize::new(1).unwrap(),
            events: NonZeroUsize::new(4).unwrap(),
            event_bytes: NonZeroUsize::new(event_bytes).unwrap(),
        },
    )
    .unwrap()
}

fn flags(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

fn registered_application() -> Call {
    Call::RegisterApplication {
        service: SERVICE.into(),
        characteristics: vec![
            (EXCHANGE_PAYLOAD.into(), flags(&["read", "notify"])),
            (CARD_EXCHANGE.into(), flags(&["write", "notify"])),
            (CHALLENGE.into(), flags(&["write", "notify"])),
            (HANDSHAKE_WRITE.into(), flags(&["write"])),
            (HANDSHAKE_NOTIFY.into(), flags(&["write", "notify"])),
            (DATA_WRITE.into(), flags(&["write-without-response"])),
            (DATA_NOTIFY.into(), flags(&["notify"])),
        ],
    }
}

async fn advertising(script: Script) -> (Bus, BleAdvertiser) {
    let mut bluez = start(script).await;
    let mut advertiser = advertiser(&bluez);
    advertiser.start(SERVICE.into(), PAYLOAD.to_vec()).unwrap();
    assert_eq!(bluez.calls.recv().await, Some(registered_application()));
    assert_eq!(
        bluez.calls.recv().await,
        Some(Call::RegisterAdvertisement {
            kind: "peripheral".into(),
            service_uuids: vec![SERVICE.into(), TOKEN.into()],
            other_properties: vec![],
        })
    );
    (bluez, advertiser)
}

fn inbound(uuid: &str, data: &[u8]) -> Event {
    Event::BleCharacteristicNotified {
        device_id: CENTRAL.into(),
        direction: BleLinkDirection::Inbound,
        uuid: uuid.into(),
        data: data.to_vec(),
    }
}

// The GATT server is up before the advertisement, so a fast central never
// discovers a service without its characteristics; the advertisement has
// no local name, only the two service UUIDs.
// @scenario: ble_exchange.feature :: BLE transport can advertise
#[tokio::test]
async fn the_gatt_server_is_registered_before_the_service_and_token_are_advertised() {
    let (_bluez, mut advertiser) = advertising(Script::default()).await;
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn the_first_subscription_reports_one_inbound_link_ready_for_gatt() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    bluez.subscribe(HANDSHAKE_NOTIFY).await.unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(Event::BleConnected {
            device_id: String::new(),
            direction: BleLinkDirection::Inbound,
        })
    );
    assert_eq!(
        advertiser.next_event().await,
        Some(Event::BleGattReady {
            device_id: String::new()
        })
    );
    bluez.subscribe(DATA_NOTIFY).await.unwrap();
    bluez
        .write(CENTRAL, HANDSHAKE_WRITE, &[1, 2, 3])
        .await
        .unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(inbound(HANDSHAKE_WRITE, &[1, 2, 3]))
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_central_write_is_reported_with_its_address_on_the_inbound_link() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    bluez.write(CENTRAL, DATA_WRITE, &[9; 178]).await.unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(inbound(DATA_WRITE, &[9; 178]))
    );
    advertiser.shutdown().await.unwrap();
}

// A peripheral has no other way to learn the MTU, and without it Core sends at
// the smallest size (vauchi/private#796).
// @internal
#[tokio::test]
async fn the_mtu_a_central_write_carries_is_reported_once_before_the_write() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    bluez
        .write_with_mtu(CENTRAL, DATA_WRITE, &[1], 247)
        .await
        .unwrap();
    bluez
        .write_with_mtu(CENTRAL, DATA_WRITE, &[2], 247)
        .await
        .unwrap();
    bluez.write(CENTRAL, DATA_WRITE, &[3]).await.unwrap();

    assert_eq!(
        advertiser.next_event().await,
        Some(Event::BleMtuNegotiated {
            device_id: CENTRAL.into(),
            mtu: 247,
        })
    );
    assert_eq!(
        advertiser.next_event().await,
        Some(inbound(DATA_WRITE, &[1]))
    );
    assert_eq!(
        advertiser.next_event().await,
        Some(inbound(DATA_WRITE, &[2]))
    );
    assert_eq!(
        advertiser.next_event().await,
        Some(inbound(DATA_WRITE, &[3]))
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_write_longer_than_an_attribute_value_is_refused_and_not_reported() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    assert_eq!(
        bluez.write(CENTRAL, HANDSHAKE_WRITE, &[7; 513]).await,
        Err("org.bluez.Error.InvalidValueLength".into())
    );
    bluez
        .write(CENTRAL, HANDSHAKE_WRITE, &[8; 512])
        .await
        .unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(inbound(HANDSHAKE_WRITE, &[8; 512]))
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_write_the_event_budget_cannot_hold_is_reported_not_dropped() {
    let mut bluez = start(Script::default()).await;
    let mut advertiser = advertiser_with(&bluez, 1024);
    advertiser.start(SERVICE.into(), PAYLOAD.to_vec()).unwrap();
    bluez.calls.recv().await;
    bluez.calls.recv().await;
    bluez.write(CENTRAL, DATA_WRITE, &[200; 512]).await.unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(Event::HardwareError {
            transport: "BLE".into(),
            error: "Bluetooth observation exceeds executor limits".into(),
        })
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_value_core_writes_on_the_inbound_link_reaches_the_central_as_a_notification() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    bluez.subscribe(HANDSHAKE_NOTIFY).await.unwrap();
    advertiser
        .notify(Notification {
            device_id: String::new(),
            uuid: HANDSHAKE_NOTIFY.into(),
            data: vec![4, 5, 6],
        })
        .unwrap();
    assert_eq!(
        bluez.notification().await,
        (HANDSHAKE_NOTIFY.into(), vec![4, 5, 6])
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn the_exchange_payload_characteristic_reads_the_advertised_payload() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    assert_eq!(bluez.read(EXCHANGE_PAYLOAD, 0).await, Ok(PAYLOAD.to_vec()));
    assert_eq!(bluez.read(EXCHANGE_PAYLOAD, 2).await, Ok(vec![0x01, 0x02]));
    assert_eq!(
        bluez.read(EXCHANGE_PAYLOAD, 5).await,
        Err("org.bluez.Error.InvalidOffset".into())
    );
    advertiser.shutdown().await.unwrap();
}

// Stopping is Core's `BleStopAdvertising`: the bus connection that owns the
// advertisement and the GATT application closes, and BlueZ releases both.
// @internal
#[tokio::test]
async fn stopping_releases_the_advertisement_and_the_gatt_server() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    let peripheral = bluez.peripheral().await;
    advertiser.stop();
    bluez.disconnected(&peripheral).await;
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn the_last_unsubscription_reports_the_inbound_link_lost() {
    let (mut bluez, mut advertiser) = advertising(Script::default()).await;
    bluez.subscribe(HANDSHAKE_NOTIFY).await.unwrap();
    advertiser.next_event().await;
    advertiser.next_event().await;
    bluez.unsubscribe(HANDSHAKE_NOTIFY).await.unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(Event::BleDisconnected {
            device_id: String::new(),
            direction: BleLinkDirection::Inbound,
            reason: "central unsubscribed".into(),
        })
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn a_refused_registration_reports_the_permission_denied() {
    let mut bluez = start(Script {
        register_error: Some("org.bluez.Error.NotAuthorized"),
        ..Script::default()
    })
    .await;
    let mut advertiser = advertiser(&bluez);
    advertiser.start(SERVICE.into(), PAYLOAD.to_vec()).unwrap();
    assert_eq!(bluez.calls.recv().await, Some(registered_application()));
    assert_eq!(
        advertiser.next_event().await,
        Some(Event::PermissionDenied {
            transport: "BLE".into()
        })
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn without_a_powered_adapter_bluetooth_is_reported_unavailable() {
    let bluez = start(Script {
        adapter_powered: Some(false),
        ..Script::default()
    })
    .await;
    let mut advertiser = advertiser(&bluez);
    advertiser.start(SERVICE.into(), PAYLOAD.to_vec()).unwrap();
    assert_eq!(
        advertiser.next_event().await,
        Some(Event::HardwareUnavailable {
            transport: "BLE".into()
        })
    );
    advertiser.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn oversized_requests_and_notifications_without_advertising_are_refused() {
    let bluez = start(Script::default()).await;
    let mut advertiser = advertiser(&bluez);
    let value = |data: Vec<u8>| Notification {
        device_id: String::new(),
        uuid: HANDSHAKE_NOTIFY.into(),
        data,
    };
    assert_eq!(
        advertiser.notify(value(vec![1])),
        Err(DeliveryError::Inactive)
    );
    assert_eq!(
        advertiser.start(SERVICE.into(), vec![0; 65]),
        Err(DeliveryError::PayloadTooLarge)
    );
    assert_eq!(
        advertiser.start("a".repeat(129), PAYLOAD.to_vec()),
        Err(DeliveryError::PayloadTooLarge)
    );
    advertiser.start(SERVICE.into(), PAYLOAD.to_vec()).unwrap();
    assert_eq!(
        advertiser.notify(value(vec![0; 513])),
        Err(DeliveryError::PayloadTooLarge)
    );
    advertiser.shutdown().await.unwrap();
}
