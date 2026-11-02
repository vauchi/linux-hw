// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The exchange's GATT central on BlueZ: one outbound link to a peer's GATT
//! server. Closing the bus connection does not disconnect a device that
//! `Device1.Connect` connected, so dropping the link asks BlueZ to.

use super::{
    BusConnection, DEVICE, OBJECT_MANAGER, Objects, PROPERTIES, Properties, call, powered_adapter,
    property, subscribe,
};
use crate::delivery::EventSender;
use crate::ports::{Central, HardwareFailure, LinkRequest, LinkRequests};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use vauchi_contract::executor::DeliveryError;
use vauchi_contract::{BleLinkDirection, Event};
use zbus::{Connection, Message, MessageStream, Variant};

const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";

pub struct BlueZCentral {
    bus: PathBuf,
}

impl BlueZCentral {
    pub fn system() -> Self {
        Self::at(super::SYSTEM_BUS)
    }

    pub fn at(bus: impl Into<PathBuf>) -> Self {
        Self { bus: bus.into() }
    }
}

impl Central for BlueZCentral {
    fn connect(
        &self,
        device_id: String,
        requests: LinkRequests,
        events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>> {
        Box::pin(link(&self.bus, device_id, requests, events))
    }
}

/// BlueZ names a device by its address, which also builds its object path,
/// so nothing else may reach the bus (DC-01).
fn is_address(device_id: &str) -> bool {
    let octets: Vec<&str> = device_id.split(':').collect();
    octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.bytes().all(|b| b.is_ascii_hexdigit()))
}

struct Characteristic {
    path: String,
    notifies: bool,
}

/// The link to one device and how Core's requests and BlueZ's signals map
/// onto its events.
struct Link {
    device_id: String,
    device: String,
    characteristics: HashMap<String, Characteristic>,
}

enum Flow {
    Continue,
    Stop,
}

async fn link(
    bus: &Path,
    device_id: String,
    mut requests: LinkRequests,
    mut events: EventSender,
) -> Result<(), HardwareFailure> {
    if !is_address(&device_id) {
        return Err(HardwareFailure::LinkFailed);
    }
    let connection = BusConnection::open(bus).await?;
    let bus_connection = &connection.connection;
    let mut signals = subscribe(bus_connection).await?;
    let adapter = powered_adapter(&managed_objects(bus_connection).await?)
        .ok_or(HardwareFailure::Unavailable)?
        .clone();
    let device = format!("{}/dev_{}", adapter.as_str(), device_id.replace(':', "_"));
    call(bus_connection, &device, DEVICE, "Connect", &())
        .await
        .map_err(link_failure)?;
    let mut connected = Connected {
        bus: bus.to_path_buf(),
        device: Some(device.clone()),
    };
    let (characteristics, mtu) = resolve(bus_connection, &mut signals, &device).await?;
    let link = Link {
        device_id,
        device,
        characteristics,
    };
    for characteristic in link.characteristics.values().filter(|c| c.notifies) {
        call(
            bus_connection,
            &characteristic.path,
            CHARACTERISTIC,
            "StartNotify",
            &(),
        )
        .await
        .map_err(link_failure)?;
    }
    if let Some(mtu) = mtu {
        let negotiated = Event::BleMtuNegotiated {
            device_id: link.device_id.clone(),
            mtu: u32::from(mtu),
        };
        if let Flow::Stop = report(&mut events, negotiated).await {
            return Ok(());
        }
    }
    if let Flow::Stop = report(&mut events, link.event_connected()).await {
        return Ok(());
    }
    loop {
        let flow = tokio::select! {
            request = requests.next() => match request {
                Some(request) => link.serve(bus_connection, request, &mut events).await,
                None => Flow::Stop,
            },
            signal = signals.next() => match signal {
                Some(Ok(signal)) => link.observe(&signal, &mut connected, &mut events).await,
                Some(Err(_)) => Flow::Continue,
                None => return Err(HardwareFailure::Failed),
            },
        };
        if let Flow::Stop = flow {
            return Ok(());
        }
    }
}

impl Link {
    fn event_connected(&self) -> Event {
        Event::BleConnected {
            device_id: self.device_id.clone(),
            direction: BleLinkDirection::Outbound,
        }
    }

    async fn serve(
        &self,
        bus: &Connection,
        request: LinkRequest,
        events: &mut EventSender,
    ) -> Flow {
        let event = match request {
            LinkRequest::Write { uuid, data } => {
                let options = HashMap::from([("type", Variant::from("request"))]);
                match self.path_of(&uuid) {
                    Some(path) => {
                        match call(bus, path, CHARACTERISTIC, "WriteValue", &(data, options)).await
                        {
                            Ok(_) => return Flow::Continue,
                            Err(_) => failure("Bluetooth write failed"),
                        }
                    }
                    None => failure("Bluetooth characteristic not found"),
                }
            }
            LinkRequest::Read { uuid } => {
                let options: HashMap<&str, Variant<'_>> = HashMap::new();
                let read = match self.path_of(&uuid) {
                    Some(path) => call(bus, path, CHARACTERISTIC, "ReadValue", &options)
                        .await
                        .ok()
                        .and_then(|reply| reply.body().deserialize::<Vec<u8>>().ok()),
                    None => None,
                };
                match read {
                    Some(data) => Event::BleCharacteristicRead {
                        device_id: self.device_id.clone(),
                        direction: BleLinkDirection::Outbound,
                        uuid,
                        data,
                    },
                    None => failure("Bluetooth read failed"),
                }
            }
        };
        report(events, event).await
    }

    async fn observe(
        &self,
        signal: &Message,
        connected: &mut Connected,
        events: &mut EventSender,
    ) -> Flow {
        let Some((path, interface, changed)) = properties_changed(signal) else {
            return Flow::Continue;
        };
        if path == self.device && interface == DEVICE {
            if property::<bool>(&changed, "Connected") != Some(false) {
                return Flow::Continue;
            }
            connected.device = None;
            let lost = Event::BleDisconnected {
                device_id: self.device_id.clone(),
                direction: BleLinkDirection::Outbound,
                reason: "the peer disconnected".into(),
            };
            report(events, lost).await;
            return Flow::Stop;
        }
        let Some(uuid) = self.uuid_at(&path).filter(|_| interface == CHARACTERISTIC) else {
            return Flow::Continue;
        };
        let Some(data) = property::<Vec<u8>>(&changed, "Value") else {
            return Flow::Continue;
        };
        let notified = Event::BleCharacteristicNotified {
            device_id: self.device_id.clone(),
            direction: BleLinkDirection::Outbound,
            uuid,
            data,
        };
        report(events, notified).await
    }

    fn path_of(&self, uuid: &str) -> Option<&str> {
        self.characteristics
            .get(&uuid.to_ascii_lowercase())
            .map(|c| c.path.as_str())
    }

    fn uuid_at(&self, path: &str) -> Option<String> {
        self.characteristics
            .iter()
            .find(|(_, c)| c.path == path)
            .map(|(uuid, _)| uuid.clone())
    }
}

/// Wait until BlueZ has resolved the device's services, then map each
/// characteristic UUID to its object, with the ATT MTU when BlueZ names it.
async fn resolve(
    bus: &Connection,
    signals: &mut MessageStream,
    device: &str,
) -> Result<(HashMap<String, Characteristic>, Option<u16>), HardwareFailure> {
    loop {
        let objects = managed_objects(bus).await?;
        let resolved = objects
            .iter()
            .find(|(path, _)| path.as_str() == device)
            .and_then(|(_, interfaces)| interfaces.get(DEVICE))
            .and_then(|properties| property::<bool>(properties, "ServicesResolved"));
        if resolved == Some(true) {
            return Ok(characteristics(&objects, device));
        }
        loop {
            let signal = signals.next().await.ok_or(HardwareFailure::LinkFailed)?;
            let Ok(signal) = signal else { continue };
            if properties_changed(&signal).is_some_and(|(path, interface, changed)| {
                path == device
                    && interface == DEVICE
                    && property::<bool>(&changed, "ServicesResolved") == Some(true)
            }) {
                break;
            }
        }
    }
}

fn characteristics(
    objects: &Objects,
    device: &str,
) -> (HashMap<String, Characteristic>, Option<u16>) {
    let prefix = format!("{device}/");
    let mut mtu = None;
    let characteristics = objects
        .iter()
        .filter(|(path, _)| path.as_str().starts_with(&prefix))
        .filter_map(|(path, interfaces)| {
            let properties = interfaces.get(CHARACTERISTIC)?;
            let uuid = property::<String>(properties, "UUID")?.to_ascii_lowercase();
            let flags = property::<Vec<String>>(properties, "Flags").unwrap_or_default();
            mtu = mtu.or(property::<u16>(properties, "MTU"));
            let notifies = flags.iter().any(|f| f == "notify" || f == "indicate");
            Some((
                uuid,
                Characteristic {
                    path: path.to_string(),
                    notifies,
                },
            ))
        })
        .collect();
    (characteristics, mtu)
}

fn properties_changed(signal: &Message) -> Option<(String, String, Properties)> {
    let header = signal.header();
    if header.interface()?.as_str() != PROPERTIES
        || header.member()?.as_str() != "PropertiesChanged"
    {
        return None;
    }
    let (interface, changed, _): (String, Properties, Vec<String>) =
        signal.body().deserialize().ok()?;
    Some((header.path()?.to_string(), interface, changed))
}

async fn managed_objects(bus: &Connection) -> Result<Objects, HardwareFailure> {
    call(bus, "/", OBJECT_MANAGER, "GetManagedObjects", &())
        .await?
        .body()
        .deserialize()
        .map_err(|_| HardwareFailure::Failed)
}

/// A refused connection or subscription is a link failure unless BlueZ
/// named a missing permission or adapter.
fn link_failure(failure: HardwareFailure) -> HardwareFailure {
    match failure {
        HardwareFailure::PermissionDenied | HardwareFailure::Unavailable => failure,
        _ => HardwareFailure::LinkFailed,
    }
}

fn failure(error: &str) -> Event {
    Event::HardwareError {
        transport: "BLE".into(),
        error: error.into(),
    }
}

/// Stop once Core no longer takes this link's events.
async fn report(events: &mut EventSender, event: Event) -> Flow {
    match events.send(event).await {
        Ok(()) | Err(DeliveryError::PayloadTooLarge | DeliveryError::InvalidEvent) => {
            Flow::Continue
        }
        Err(DeliveryError::Inactive | DeliveryError::Closed | DeliveryError::Full) => Flow::Stop,
    }
}

/// Disconnects the device when the link ends without the peer having
/// dropped it, on a connection of its own: the link's socket is shut by then.
struct Connected {
    bus: PathBuf,
    device: Option<String>,
}

impl Drop for Connected {
    fn drop(&mut self) {
        let (Some(device), Ok(runtime)) =
            (self.device.take(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        let bus = self.bus.clone();
        runtime.spawn(async move {
            if let Ok(connection) = BusConnection::open(&bus).await {
                let _ = call(&connection.connection, &device, DEVICE, "Disconnect", &()).await;
            }
        });
    }
}
