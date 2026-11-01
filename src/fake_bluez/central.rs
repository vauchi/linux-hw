// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! BlueZ's side of a registered peripheral: reading the application and the
//! advertisement the way `bluetoothd` does on registration, and a remote
//! central subscribing, writing and reading over the GATT server.

use super::{Call, FakeBlueZ, Objects, Properties, device_path};
use futures_util::StreamExt;
use std::collections::HashMap;
use zbus::message::Type;
use zbus::{Connection, Message, MessageStream, OwnedObjectPath, Variant};

const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// A registered GATT application: its owner's bus name, its characteristics
/// by UUID, and the value changes it emits (notifications).
pub struct App {
    owner: String,
    characteristics: HashMap<String, OwnedObjectPath>,
    notifications: MessageStream,
}

pub(super) async fn read_application(bluez: &Connection, owner: &str, root: &str) -> (App, Call) {
    // Subscribe before reading, so no value change emitted afterwards is lost.
    let notifications = MessageStream::for_match_rule(
        zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(owner)
            .interface(PROPERTIES)
            .member("PropertiesChanged")
            .build()
            .unwrap(),
        bluez,
        None,
    )
    .await
    .unwrap();
    let objects: Objects = bluez
        .call_method(
            Some(owner),
            root,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let mut service = None;
    let mut listed = Vec::new();
    let mut characteristics = HashMap::new();
    for (path, interfaces) in objects {
        if let Some(properties) = interfaces.get("org.bluez.GattService1") {
            service = Some(value::<String>(properties, "UUID"));
        }
        if let Some(properties) = interfaces.get(CHARACTERISTIC) {
            let uuid = value::<String>(properties, "UUID");
            listed.push((uuid.clone(), value::<Vec<String>>(properties, "Flags")));
            characteristics.insert(uuid, path);
        }
    }
    listed.sort();
    let app = App {
        owner: owner.into(),
        characteristics,
        notifications,
    };
    let call = Call::RegisterApplication {
        service: service.expect("the application exports a GattService1"),
        characteristics: listed,
    };
    (app, call)
}

pub(super) async fn read_advertisement(bluez: &Connection, owner: &str, path: &str) -> Call {
    let mut properties: Properties = bluez
        .call_method(
            Some(owner),
            path,
            Some(PROPERTIES),
            "GetAll",
            &("org.bluez.LEAdvertisement1",),
        )
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let kind = value::<String>(&properties, "Type");
    let service_uuids = value::<Vec<String>>(&properties, "ServiceUUIDs");
    properties.remove("Type");
    properties.remove("ServiceUUIDs");
    let mut other_properties: Vec<String> = properties.into_keys().collect();
    other_properties.sort();
    Call::RegisterAdvertisement {
        kind,
        service_uuids,
        other_properties,
    }
}

fn value<T: TryFrom<zbus::OwnedVariant>>(properties: &Properties, name: &str) -> T
where
    T::Error: std::fmt::Debug,
{
    properties[name].try_clone().unwrap().try_into().unwrap()
}

impl FakeBlueZ {
    /// The bus name of the peripheral whose application registered.
    pub async fn peripheral(&mut self) -> String {
        self.app().await.owner.clone()
    }

    /// A central enables notifications on `uuid` (writes its CCC descriptor).
    pub async fn subscribe(&mut self, uuid: &str) -> Result<(), String> {
        self.call_characteristic(uuid, "StartNotify", &())
            .await
            .map(drop)
    }

    pub async fn unsubscribe(&mut self, uuid: &str) -> Result<(), String> {
        self.call_characteristic(uuid, "StopNotify", &())
            .await
            .map(drop)
    }

    /// The central at `address` writes `data` to `uuid`; `Err` holds the
    /// D-Bus error name the peripheral answered with.
    pub async fn write(&mut self, address: &str, uuid: &str, data: &[u8]) -> Result<(), String> {
        let options = HashMap::from([
            ("device", Variant::from(device_path(address))),
            ("type", Variant::from("request")),
        ]);
        self.call_characteristic(uuid, "WriteValue", &(data, options))
            .await
            .map(drop)
    }

    pub async fn read(&mut self, uuid: &str, offset: u16) -> Result<Vec<u8>, String> {
        let options = HashMap::from([("offset", Variant::from(offset))]);
        let reply = self
            .call_characteristic(uuid, "ReadValue", &(options,))
            .await?;
        Ok(reply.body().deserialize().unwrap())
    }

    /// The next value the peripheral notified, with its characteristic UUID.
    pub async fn notification(&mut self) -> (String, Vec<u8>) {
        let app = self.app().await;
        while let Some(message) = app.notifications.next().await {
            let message = message.unwrap();
            let Some(path) = message.header().path().map(ToString::to_string) else {
                continue;
            };
            let (interface, mut changed, _): (String, Properties, Vec<String>) =
                message.body().deserialize().unwrap();
            let Some(value) = changed.remove("Value") else {
                continue;
            };
            if interface != CHARACTERISTIC {
                continue;
            }
            let uuid = app
                .characteristics
                .iter()
                .find(|(_, p)| p.as_str() == path)
                .map(|(uuid, _)| uuid.clone())
                .expect("a value change on a characteristic the application listed");
            return (uuid, value.try_into().unwrap());
        }
        panic!("bus closed before a notification");
    }

    async fn app(&mut self) -> &mut App {
        if self.app.is_none() {
            self.app = Some(self.apps.recv().await.expect("an application registers"));
        }
        self.app.as_mut().unwrap()
    }

    async fn call_characteristic<B>(
        &mut self,
        uuid: &str,
        method: &str,
        body: &B,
    ) -> Result<Message, String>
    where
        B: zbus::export::serde::Serialize + zbus::DynamicType,
    {
        let bluez = self.bluez.clone();
        let app = self.app().await;
        let path = app.characteristics[uuid].clone();
        bluez
            .call_method(
                Some(app.owner.as_str()),
                path.as_str(),
                Some(CHARACTERISTIC),
                method,
                body,
            )
            .await
            .map_err(|error| match error {
                zbus::Error::MethodError(name, _, _) => name.to_string(),
                other => other.to_string(),
            })
    }
}
