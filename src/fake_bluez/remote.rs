// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A remote GATT server, as BlueZ shows one to a central: a device that
//! connects, resolves its services, and answers reads, writes and
//! subscriptions on its characteristics. The peer itself is scripted.

use super::{Call, Objects, Properties, device_path};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::{Connection, Message, ObjectPath, OwnedObjectPath, Variant};

const DEVICE: &str = "org.bluez.Device1";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";
/// The ATT MTU every remote reports, as BlueZ does on its characteristics.
pub const REMOTE_MTU: u16 = 185;

pub struct RemoteCharacteristic {
    pub uuid: String,
    pub flags: Vec<String>,
    /// What a read returns.
    pub value: Vec<u8>,
}

impl RemoteCharacteristic {
    pub fn new(uuid: &str, flags: &[&str], value: &[u8]) -> Self {
        Self {
            uuid: uuid.into(),
            flags: flags.iter().map(ToString::to_string).collect(),
            value: value.to_vec(),
        }
    }
}

#[derive(Default)]
pub(super) struct Remote {
    characteristics: Vec<RemoteCharacteristic>,
    connected: bool,
}

#[derive(Clone, Default)]
pub(super) struct Remotes(Arc<Mutex<HashMap<String, Remote>>>);

impl Remotes {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Remote>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn add(&self, address: &str, characteristics: Vec<RemoteCharacteristic>) {
        self.lock().insert(
            address.into(),
            Remote {
                characteristics,
                connected: false,
            },
        );
    }

    /// The remote's device object, and its service and characteristics once
    /// connected, the way `GetManagedObjects` lists them.
    pub(super) fn list(&self, objects: &mut Objects) {
        for (address, remote) in self.lock().iter() {
            objects.insert(
                device_path(address),
                HashMap::from([(
                    DEVICE.into(),
                    properties([
                        ("Address", Variant::from(address.as_str())),
                        ("Connected", Variant::from(remote.connected)),
                        ("ServicesResolved", Variant::from(remote.connected)),
                    ]),
                )]),
            );
            if !remote.connected {
                continue;
            }
            for (index, characteristic) in remote.characteristics.iter().enumerate() {
                objects.insert(
                    characteristic_path(address, index),
                    HashMap::from([(
                        CHARACTERISTIC.into(),
                        properties([
                            ("UUID", Variant::from(characteristic.uuid.as_str())),
                            ("Flags", Variant::from(characteristic.flags.clone())),
                            ("MTU", Variant::from(REMOTE_MTU)),
                        ]),
                    )]),
                );
            }
        }
    }

    fn set_connected(&self, address: &str, connected: bool) -> bool {
        match self.lock().get_mut(address) {
            Some(remote) => {
                remote.connected = connected;
                true
            }
            None => false,
        }
    }

    fn characteristic(&self, path: &str) -> Option<(String, String, Vec<u8>)> {
        self.lock().iter().find_map(|(address, remote)| {
            remote
                .characteristics
                .iter()
                .enumerate()
                .find(|(index, _)| characteristic_path(address, *index).as_str() == path)
                .map(|(_, c)| (address.clone(), c.uuid.clone(), c.value.clone()))
        })
    }

    pub(super) fn path_of(&self, address: &str, uuid: &str) -> OwnedObjectPath {
        let remotes = self.lock();
        let index = remotes[address]
            .characteristics
            .iter()
            .position(|c| c.uuid == uuid)
            .expect("the remote has the characteristic");
        characteristic_path(address, index)
    }

    /// Answer a call on a remote's device or characteristic object; `None`
    /// when `path` is not one of them.
    pub(super) async fn answer(
        &self,
        bluez: &Connection,
        message: &Message,
        connect_error: Option<&str>,
        calls: &mpsc::UnboundedSender<Call>,
    ) -> Option<zbus::Result<()>> {
        let header = message.header();
        let path = header.path()?.to_string();
        let member = header.member()?.to_string();
        if let Some(address) = self.address_of_device(&path) {
            return match member.as_str() {
                "Connect" => Some(
                    self.connect(bluez, &header, &address, connect_error, calls)
                        .await,
                ),
                "Disconnect" => {
                    calls
                        .send(Call::Disconnect {
                            address: address.clone(),
                        })
                        .unwrap();
                    self.set_connected(&address, false);
                    let replied = bluez.reply(&header, &()).await.map(|_| ());
                    emit_connected(bluez, &address, false).await;
                    Some(replied)
                }
                _ => None,
            };
        }
        let (_, uuid, value) = self.characteristic(&path)?;
        let replied = match member.as_str() {
            "ReadValue" => {
                calls.send(Call::ReadValue { uuid }).unwrap();
                bluez.reply(&header, &value).await
            }
            "WriteValue" => {
                let (data, _): (Vec<u8>, Properties) = message.body().deserialize().unwrap();
                calls.send(Call::WriteValue { uuid, data }).unwrap();
                bluez.reply(&header, &()).await
            }
            "StartNotify" => {
                calls.send(Call::StartNotify { uuid }).unwrap();
                bluez.reply(&header, &()).await
            }
            _ => return None,
        };
        Some(replied.map(|_| ()))
    }

    async fn connect(
        &self,
        bluez: &Connection,
        header: &Header<'_>,
        address: &str,
        connect_error: Option<&str>,
        calls: &mpsc::UnboundedSender<Call>,
    ) -> zbus::Result<()> {
        calls
            .send(Call::Connect {
                address: address.into(),
            })
            .unwrap();
        if let Some(error) = connect_error {
            return bluez
                .reply_error(header, error, &"scripted")
                .await
                .map(|_| ());
        }
        self.set_connected(address, true);
        bluez.reply(header, &()).await?;
        emit_connected(bluez, address, true).await;
        Ok(())
    }

    pub(super) async fn drop_link(&self, bluez: &Connection, address: &str) {
        self.set_connected(address, false);
        emit_connected(bluez, address, false).await;
    }

    fn address_of_device(&self, path: &str) -> Option<String> {
        self.lock()
            .keys()
            .find(|address| device_path(address).as_str() == path)
            .cloned()
    }
}

pub(super) async fn emit_value(bluez: &Connection, path: &OwnedObjectPath, data: &[u8]) {
    let changed = HashMap::from([("Value", Variant::from(data.to_vec()))]);
    bluez
        .emit_signal(
            None::<&str>,
            path.as_str(),
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(CHARACTERISTIC, changed, Vec::<String>::new()),
        )
        .await
        .unwrap();
}

async fn emit_connected(bluez: &Connection, address: &str, connected: bool) {
    let changed = HashMap::from([
        ("Connected", Variant::from(connected)),
        ("ServicesResolved", Variant::from(connected)),
    ]);
    bluez
        .emit_signal(
            None::<&str>,
            device_path(address).as_str(),
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(DEVICE, changed, Vec::<String>::new()),
        )
        .await
        .unwrap();
}

fn characteristic_path(address: &str, index: usize) -> OwnedObjectPath {
    let device = device_path(address);
    ObjectPath::try_from(format!(
        "{}/service0001/char{:04x}",
        device.as_str(),
        index + 1
    ))
    .unwrap()
    .into()
}

fn properties<const N: usize>(entries: [(&str, Variant<'_>); N]) -> Properties {
    entries
        .into_iter()
        .map(|(name, value)| (name.into(), value.try_into().unwrap()))
        .collect()
}
