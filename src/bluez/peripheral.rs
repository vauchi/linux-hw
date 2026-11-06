// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The exchange's GATT peripheral on BlueZ: one advertisement and one GATT
//! application, both owned by the bus connection of one operation, so
//! dropping the operation withdraws them (BlueZ releases the objects of a
//! client that disconnects).

use super::{
    BusConnection, DEVICE, MAX_TRACKED_DEVICES, OBJECT_MANAGER, Objects, PROPERTIES, Properties,
    call, powered_adapter, property,
};
use crate::delivery::EventSender;
use crate::ports::{Advertising, HardwareFailure, Notification, Outbound};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use uuid::Uuid;
use vauchi_contract::executor::DeliveryError;
use vauchi_contract::{BleLinkDirection, Event};
use zbus::message::Type;

use zbus::{Connection, Message, MessageStream, ObjectPath, OwnedObjectPath, Variant};

const APPLICATION: &str = "/app/vauchi";
const SERVICE: &str = "/app/vauchi/service";
const ADVERTISEMENT: &str = "/app/vauchi/advertisement";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";
const LE_ADVERTISEMENT: &str = "org.bluez.LEAdvertisement1";
const EXCHANGE_PAYLOAD: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567891";
/// The longest attribute value ATT can carry.
const MAX_VALUE: usize = 512;
/// The characteristics and BlueZ flags the phones' GATT servers expose
/// (android `BleUuids.kt`), so a phone's central finds the same service.
const CHARACTERISTICS: [(&str, &[&str]); 7] = [
    (EXCHANGE_PAYLOAD, &["read", "notify"]),
    ("a1b2c3d4-e5f6-7890-abcd-ef1234567892", &["write", "notify"]),
    ("a1b2c3d4-e5f6-7890-abcd-ef1234567893", &["write", "notify"]),
    ("a1b2c3d4-e5f6-7890-abcd-ef1234567894", &["write"]),
    ("a1b2c3d4-e5f6-7890-abcd-ef1234567895", &["write", "notify"]),
    (
        "a1b2c3d4-e5f6-7890-abcd-ef1234567896",
        &["write-without-response"],
    ),
    ("a1b2c3d4-e5f6-7890-abcd-ef1234567897", &["notify"]),
];

pub struct BlueZAdvertising {
    bus: PathBuf,
}

impl BlueZAdvertising {
    pub fn system() -> Self {
        Self::at(super::SYSTEM_BUS)
    }

    pub fn at(bus: impl Into<PathBuf>) -> Self {
        Self { bus: bus.into() }
    }
}

impl Advertising for BlueZAdvertising {
    fn advertise(
        &self,
        service_uuid: String,
        payload: Vec<u8>,
        outbound: Outbound,
        events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(advertise(
            &self.bus,
            service_uuid,
            payload,
            outbound,
            events,
        ))
    }
}

/// Core's token as the phones advertise it: its first two bytes, big-endian,
/// as a 16-bit Bluetooth-base service UUID, the only token channel iOS
/// peripherals have (android `BleUuids.tokenToServiceUuid`).
fn token_service_uuid(payload: &[u8]) -> String {
    let byte = |index: usize| payload.get(index).copied().unwrap_or(0);
    let short = u16::from_be_bytes([byte(0), byte(1)]);
    format!("0000{short:04x}-0000-1000-8000-00805f9b34fb")
}

enum Flow {
    Continue,
    Stop,
}

async fn advertise(
    bus: &Path,
    service_uuid: String,
    payload: Vec<u8>,
    mut outbound: Outbound,
    mut events: EventSender,
) -> Result<(), HardwareFailure> {
    let service = Uuid::parse_str(&service_uuid).map_err(|_| HardwareFailure::Failed)?;
    let connection = BusConnection::open(bus).await?;
    let bus = &connection.connection;
    // Before registering: bluetoothd reads the objects back before it
    // answers, and those calls must not be missed.
    let mut incoming = MessageStream::from(bus);
    let objects: Objects = call(bus, "/", OBJECT_MANAGER, "GetManagedObjects", &())
        .await?
        .body()
        .deserialize()
        .map_err(|_| HardwareFailure::Failed)?;
    let adapter = powered_adapter(&objects)
        .ok_or(HardwareFailure::Unavailable)?
        .clone();
    let mut peripheral = Peripheral {
        service: service.to_string(),
        token: token_service_uuid(&payload),
        payload,
        subscribed: HashSet::new(),
        addresses: HashMap::new(),
        reported_mtu: None,
    };
    let registration = register(bus, adapter.as_str());
    tokio::pin!(registration);
    let mut registered = false;
    loop {
        tokio::select! {
            result = &mut registration, if !registered => {
                result?;
                registered = true;
            }
            message = incoming.next() => {
                let Some(message) = message else {
                    return Err(HardwareFailure::Failed);
                };
                let Ok(message) = message else { continue };
                if let Flow::Stop = peripheral.serve(bus, &message, &mut events).await? {
                    return Ok(());
                }
            }
            notification = outbound.next() => {
                let Some(notification) = notification else { return Ok(()) };
                peripheral.notify(bus, notification).await;
            }
        }
    }
}

async fn register(bus: &Connection, adapter: &str) -> Result<(), HardwareFailure> {
    let options: HashMap<&str, Variant<'_>> = HashMap::new();
    call(
        bus,
        adapter,
        "org.bluez.GattManager1",
        "RegisterApplication",
        &(ObjectPath::from_static_str_unchecked(APPLICATION), &options),
    )
    .await?;
    call(
        bus,
        adapter,
        "org.bluez.LEAdvertisingManager1",
        "RegisterAdvertisement",
        &(
            ObjectPath::from_static_str_unchecked(ADVERTISEMENT),
            &options,
        ),
    )
    .await?;
    Ok(())
}

fn characteristic_path(index: usize) -> String {
    format!("{SERVICE}/char{index}")
}

struct Peripheral {
    service: String,
    token: String,
    payload: Vec<u8>,
    subscribed: HashSet<usize>,
    addresses: HashMap<OwnedObjectPath, String>,
    reported_mtu: Option<u16>,
}

impl Peripheral {
    async fn serve(
        &mut self,
        bus: &Connection,
        message: &Message,
        events: &mut EventSender,
    ) -> Result<Flow, HardwareFailure> {
        let header = message.header();
        if header.message_type() != Type::MethodCall {
            return Ok(Flow::Continue);
        }
        let path = header.path().map(ToString::to_string).unwrap_or_default();
        let member = header.member().map(ToString::to_string).unwrap_or_default();
        let characteristic = (0..CHARACTERISTICS.len()).find(|&i| characteristic_path(i) == path);
        let replied = match (path.as_str(), member.as_str(), characteristic) {
            (APPLICATION, "GetManagedObjects", _) => bus.reply(&header, &self.objects()).await,
            (ADVERTISEMENT, "GetAll", _) => {
                let interface: (String,) = message.body().deserialize().unwrap_or_default();
                bus.reply(&header, &self.advertisement(&interface.0)).await
            }
            // BlueZ withdrew the advertisement itself, e.g. the adapter
            // went off; the operation cannot go on.
            (ADVERTISEMENT, "Release", _) => {
                let _ = bus.reply(&header, &()).await;
                return Err(HardwareFailure::AdvertisingFailed);
            }
            (_, "ReadValue", Some(index)) => self.read(bus, message, index).await,
            (_, "WriteValue", Some(index)) => {
                return self.write(bus, message, index, events).await;
            }
            (_, "StartNotify", Some(index)) => {
                bus.reply(&header, &()).await.ok();
                return Ok(self.subscribe(index, events).await);
            }
            (_, "StopNotify", Some(index)) => {
                bus.reply(&header, &()).await.ok();
                return Ok(self.unsubscribe(index, events).await);
            }
            _ => {
                bus.reply_error(&header, "org.freedesktop.DBus.Error.UnknownMethod", &"")
                    .await
            }
        };
        replied.map_err(|_| HardwareFailure::AdvertisingFailed)?;
        Ok(Flow::Continue)
    }

    fn objects(&self) -> HashMap<ObjectPath<'_>, HashMap<&str, HashMap<&str, Variant<'_>>>> {
        let mut objects = HashMap::from([(
            ObjectPath::from_static_str_unchecked(SERVICE),
            HashMap::from([(
                "org.bluez.GattService1",
                HashMap::from([
                    ("UUID", Variant::from(self.service.as_str())),
                    ("Primary", Variant::from(true)),
                ]),
            )]),
        )]);
        for (index, (uuid, flags)) in CHARACTERISTICS.iter().enumerate() {
            let Ok(path) = ObjectPath::try_from(characteristic_path(index)) else {
                continue;
            };
            objects.insert(
                path,
                HashMap::from([(
                    CHARACTERISTIC,
                    HashMap::from([
                        ("UUID", Variant::from(*uuid)),
                        (
                            "Service",
                            Variant::from(ObjectPath::from_static_str_unchecked(SERVICE)),
                        ),
                        ("Flags", Variant::from(flags.to_vec())),
                    ]),
                )]),
            );
        }
        objects
    }

    /// Only the two service UUIDs: no local name, so the advertisement
    /// carries nothing that names the user or the device.
    fn advertisement(&self, interface: &str) -> HashMap<&str, Variant<'_>> {
        if interface != LE_ADVERTISEMENT {
            return HashMap::new();
        }
        HashMap::from([
            ("Type", Variant::from("peripheral")),
            (
                "ServiceUUIDs",
                Variant::from(vec![self.service.as_str(), self.token.as_str()]),
            ),
        ])
    }

    async fn read(&self, bus: &Connection, message: &Message, index: usize) -> zbus::Result<()> {
        let header = message.header();
        let (options,): (Properties,) = message.body().deserialize().unwrap_or_default();
        let offset = usize::from(property::<u16>(&options, "offset").unwrap_or(0));
        let value: &[u8] = if CHARACTERISTICS[index].0 == EXCHANGE_PAYLOAD {
            &self.payload
        } else {
            &[]
        };
        match value.get(offset..) {
            Some(rest) => bus.reply(&header, &rest.to_vec()).await,
            None => {
                bus.reply_error(&header, "org.bluez.Error.InvalidOffset", &"")
                    .await
            }
        }
    }

    async fn write(
        &mut self,
        bus: &Connection,
        message: &Message,
        index: usize,
        events: &mut EventSender,
    ) -> Result<Flow, HardwareFailure> {
        let header = message.header();
        let Ok((data, options)) = message.body().deserialize::<(Vec<u8>, Properties)>() else {
            bus.reply_error(&header, "org.bluez.Error.Failed", &"")
                .await
                .ok();
            return Ok(Flow::Continue);
        };
        if data.len() > MAX_VALUE {
            bus.reply_error(&header, "org.bluez.Error.InvalidValueLength", &"")
                .await
                .ok();
            return Ok(Flow::Continue);
        }
        bus.reply(&header, &()).await.ok();
        let device_id = match property::<OwnedObjectPath>(&options, "device") {
            Some(device) => self.address(bus, device).await,
            None => String::new(),
        };
        // BlueZ tells a GATT server the exchanged MTU only in a write's options.
        if let Some(mtu) = property::<u16>(&options, "mtu")
            && self.reported_mtu != Some(mtu)
        {
            self.reported_mtu = Some(mtu);
            let negotiated = Event::BleMtuNegotiated {
                device_id: device_id.clone(),
                mtu: u32::from(mtu),
            };
            if let Flow::Stop = deliver(events, negotiated).await {
                return Ok(Flow::Stop);
            }
        }
        Ok(deliver(
            events,
            Event::BleCharacteristicNotified {
                device_id,
                direction: BleLinkDirection::Inbound,
                uuid: CHARACTERISTICS[index].0.into(),
                data,
            },
        )
        .await)
    }

    /// The writer's address, the device id the phones report. Unknown when
    /// BlueZ does not answer: an empty id is Core's wildcard for the link.
    async fn address(&mut self, bus: &Connection, device: OwnedObjectPath) -> String {
        if let Some(address) = self.addresses.get(&device) {
            return address.clone();
        }
        let address = match call(
            bus,
            device.as_str(),
            PROPERTIES,
            "Get",
            &(DEVICE, "Address"),
        )
        .await
        {
            Ok(reply) => reply
                .body()
                .deserialize::<zbus::OwnedVariant>()
                .ok()
                .and_then(|value| String::try_from(value).ok())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        if !address.is_empty() && self.addresses.len() < MAX_TRACKED_DEVICES {
            self.addresses.insert(device, address.clone());
        }
        address
    }

    /// BlueZ calls `StartNotify` once, for the first central to subscribe,
    /// and names none, so the link is reported unaddressed; the writes that
    /// follow carry the address.
    async fn subscribe(&mut self, index: usize, events: &mut EventSender) -> Flow {
        let first = self.subscribed.is_empty();
        self.subscribed.insert(index);
        if !first {
            return Flow::Continue;
        }
        let connected = Event::BleConnected {
            device_id: String::new(),
            direction: BleLinkDirection::Inbound,
        };
        if let Flow::Stop = deliver(events, connected).await {
            return Flow::Stop;
        }
        deliver(
            events,
            Event::BleGattReady {
                device_id: String::new(),
            },
        )
        .await
    }

    async fn unsubscribe(&mut self, index: usize, events: &mut EventSender) -> Flow {
        if !self.subscribed.remove(&index) || !self.subscribed.is_empty() {
            return Flow::Continue;
        }
        deliver(
            events,
            Event::BleDisconnected {
                device_id: String::new(),
                direction: BleLinkDirection::Inbound,
                reason: "central unsubscribed".into(),
            },
        )
        .await
    }

    /// Notified to every subscribed central: `StartNotify` gives no handle
    /// on one of them, so `device_id` cannot narrow it.
    async fn notify(&self, bus: &Connection, notification: Notification) {
        let Some(index) = CHARACTERISTICS.iter().position(|(uuid, flags)| {
            uuid.eq_ignore_ascii_case(&notification.uuid) && flags.contains(&"notify")
        }) else {
            return;
        };
        let changed = HashMap::from([("Value", Variant::from(notification.data))]);
        let _ = bus
            .emit_signal(
                None::<&str>,
                characteristic_path(index).as_str(),
                PROPERTIES,
                "PropertiesChanged",
                &(CHARACTERISTIC, changed, Vec::<String>::new()),
            )
            .await;
    }
}

/// A central's write the inbox cannot hold is reported, not dropped: a lost
/// chunk would stall the exchange with nothing to say why.
async fn deliver(events: &mut EventSender, event: Event) -> Flow {
    let refused = match events.send(event).await {
        Ok(()) | Err(DeliveryError::InvalidEvent) => return Flow::Continue,
        Err(DeliveryError::PayloadTooLarge) => {
            events
                .send(HardwareFailure::InvalidObservation.event())
                .await
        }
        Err(error) => Err(error),
    };
    match refused {
        Ok(()) | Err(DeliveryError::PayloadTooLarge | DeliveryError::InvalidEvent) => {
            Flow::Continue
        }
        Err(DeliveryError::Inactive | DeliveryError::Closed | DeliveryError::Full) => Flow::Stop,
    }
}
