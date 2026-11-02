// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A scripted `org.bluez` on a private dbus-daemon: the D-Bus conversation is
//! real, only the radio is simulated. Simulated scans do not certify Bluetooth
//! access; native acceptance does (private#770).
//!
//! Test support only, behind the `fake-bluez` feature: it panics on any
//! unexpected bus traffic. Shared so that every host of `BlueZDiscovery` (this
//! crate's tests, GTK, later Qt and the terminals) runs against one fake.

mod central;
mod remote;

pub use remote::{REMOTE_MTU, RemoteCharacteristic};

use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zbus::message::Type;
use zbus::{Connection, MessageStream, ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

pub const ADAPTER: &str = "/org/bluez/hci0";
type Properties = HashMap<String, OwnedVariant>;
type Objects = HashMap<OwnedObjectPath, HashMap<String, Properties>>;

#[derive(Debug, PartialEq)]
pub enum Call {
    SetDiscoveryFilter {
        uuids: Vec<String>,
        transport: String,
    },
    StartDiscovery,
    StopDiscovery,
    /// The GATT service UUID and each characteristic's UUID and flags, sorted
    /// by UUID, as read back from the registered application.
    RegisterApplication {
        service: String,
        characteristics: Vec<(String, Vec<String>)>,
    },
    /// The advertisement as read back on registration; `other_properties`
    /// names every property besides `Type` and `ServiceUUIDs`.
    RegisterAdvertisement {
        kind: String,
        service_uuids: Vec<String>,
        other_properties: Vec<String>,
    },
    /// A central's calls on a remote GATT server (`remote.rs`).
    Connect {
        address: String,
    },
    Disconnect {
        address: String,
    },
    StartNotify {
        uuid: String,
    },
    WriteValue {
        uuid: String,
        data: Vec<u8>,
    },
    ReadValue {
        uuid: String,
    },
}

pub struct Script {
    pub owns_name: bool,
    pub adapter_powered: Option<bool>,
    pub start_error: Option<&'static str>,
    /// Refuses `RegisterApplication` with this D-Bus error name.
    pub register_error: Option<&'static str>,
    /// Refuses a remote's `Device1.Connect` with this D-Bus error name.
    pub connect_error: Option<&'static str>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            owns_name: true,
            adapter_powered: Some(true),
            start_error: None,
            register_error: None,
            connect_error: None,
        }
    }
}

pub struct FakeBlueZ {
    daemon: Child,
    socket: PathBuf,
    bluez: Connection,
    pub calls: mpsc::UnboundedReceiver<Call>,
    scanner: mpsc::UnboundedReceiver<String>,
    gone: MessageStream,
    responder: JoinHandle<()>,
    apps: mpsc::UnboundedReceiver<central::App>,
    app: Option<central::App>,
    remotes: remote::Remotes,
}

impl FakeBlueZ {
    /// Starts the private bus with its socket in `directory`, which the
    /// caller owns and keeps until the fake is dropped: a fresh
    /// `tempfile::tempdir()`, never a predictable path (semgrep
    /// `vauchi-no-env-temp-dir`). Taking it keeps tempfile out of this
    /// crate's dependencies (VRS06).
    pub async fn start(script: Script, directory: &Path) -> Self {
        let socket = directory.join("bus");
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is required for the BlueZ contract tests");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim();
        let bluez = zbus::connection::Builder::address(address)
            .build()
            .await
            .unwrap();
        if script.owns_name {
            bluez.request_name("org.bluez").await.unwrap();
        }
        let gone = MessageStream::for_match_rule(
            zbus::MatchRule::builder()
                .msg_type(Type::Signal)
                .member("NameOwnerChanged")
                .build()
                .unwrap(),
            &bluez,
            None,
        )
        .await
        .unwrap();
        let (calls_tx, calls) = mpsc::unbounded_channel();
        let (scanner_tx, scanner) = mpsc::unbounded_channel();
        let (apps_tx, apps) = mpsc::unbounded_channel();
        let remotes = remote::Remotes::default();
        let responder = tokio::spawn(respond(
            bluez.clone(),
            script,
            Peers {
                calls: calls_tx,
                scanner: scanner_tx,
                apps: apps_tx,
                remotes: remotes.clone(),
            },
        ));
        Self {
            daemon,
            socket,
            bluez,
            calls,
            scanner,
            gone,
            responder,
            apps,
            app: None,
            remotes,
        }
    }

    /// A GATT server in range at `address`, for the central side to connect to.
    pub fn remote(&self, address: &str, characteristics: Vec<RemoteCharacteristic>) {
        self.remotes.add(address, characteristics);
    }

    /// The remote changes `uuid`'s value, as a notification to subscribers.
    pub async fn remote_notifies(&self, address: &str, uuid: &str, data: &[u8]) {
        remote::emit_value(&self.bluez, &self.remotes.path_of(address, uuid), data).await;
    }

    /// The remote goes out of range or drops the link.
    pub async fn remote_drops(&self, address: &str) {
        self.remotes.drop_link(&self.bluez, address).await;
    }

    pub fn socket(&self) -> PathBuf {
        self.socket.clone()
    }

    pub async fn device_found(&self, address: &str, rssi: i16, uuids: &[&str]) {
        self.device_added(address, Some(rssi), uuids).await;
    }

    /// BlueZ announces devices it remembers from earlier scans without RSSI.
    pub async fn device_remembered(&self, address: &str, uuids: &[&str]) {
        self.device_added(address, None, uuids).await;
    }

    async fn device_added(&self, address: &str, rssi: Option<i16>, uuids: &[&str]) {
        let mut interfaces = HashMap::new();
        interfaces.insert("org.bluez.Device1", device(address, rssi, uuids));
        self.bluez
            .emit_signal(
                None::<&str>,
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "InterfacesAdded",
                &(device_path(address), interfaces),
            )
            .await
            .unwrap();
    }

    pub async fn rssi_changed(&self, address: &str, rssi: i16) {
        let changed = HashMap::from([("RSSI", Variant::from(rssi))]);
        self.bluez
            .emit_signal(
                None::<&str>,
                device_path(address).as_str(),
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &("org.bluez.Device1", changed, Vec::<String>::new()),
            )
            .await
            .unwrap();
    }

    /// The scan's unique bus name, as seen on its `StartDiscovery` call.
    pub async fn scanner(&mut self) -> String {
        self.scanner.recv().await.unwrap()
    }

    pub async fn disconnected(&mut self, name: &str) {
        while let Some(message) = self.gone.next().await {
            let (owner, _, new): (String, String, String) =
                message.unwrap().body().deserialize().unwrap();
            if owner == name && new.is_empty() {
                return;
            }
        }
        panic!("bus closed before {name} disconnected");
    }

    pub fn stop_bus(&mut self) {
        self.daemon.kill().unwrap();
        self.daemon.wait().unwrap();
    }
}

impl Drop for FakeBlueZ {
    fn drop(&mut self) {
        self.responder.abort();
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn device_path(address: &str) -> OwnedObjectPath {
    ObjectPath::try_from(format!("{ADAPTER}/dev_{}", address.replace(':', "_")))
        .unwrap()
        .into()
}

fn device(address: &str, rssi: Option<i16>, uuids: &[&str]) -> Properties {
    let uuids: Vec<String> = uuids.iter().map(ToString::to_string).collect();
    let mut properties = HashMap::from([
        ("Address".into(), Variant::from(address).try_into().unwrap()),
        ("UUIDs".into(), Variant::from(uuids).try_into().unwrap()),
    ]);
    if let Some(rssi) = rssi {
        properties.insert("RSSI".into(), Variant::from(rssi).try_into().unwrap());
    }
    properties
}

fn objects(script: &Script, remotes: &remote::Remotes) -> Objects {
    let Some(powered) = script.adapter_powered else {
        return Objects::new();
    };
    let adapter = HashMap::from([("Powered".into(), Variant::from(powered).try_into().unwrap())]);
    let mut objects = HashMap::from([(
        ObjectPath::try_from(ADAPTER).unwrap().into(),
        HashMap::from([
            ("org.bluez.Adapter1".into(), adapter),
            ("org.bluez.GattManager1".into(), Properties::new()),
            ("org.bluez.LEAdvertisingManager1".into(), Properties::new()),
        ]),
    )]);
    remotes.list(&mut objects);
    objects
}

struct Peers {
    calls: mpsc::UnboundedSender<Call>,
    scanner: mpsc::UnboundedSender<String>,
    apps: mpsc::UnboundedSender<central::App>,
    remotes: remote::Remotes,
}

async fn respond(bluez: Connection, script: Script, peers: Peers) {
    let Peers {
        calls,
        scanner,
        apps,
        remotes,
    } = peers;
    let mut incoming = MessageStream::from(&bluez);
    while let Some(Ok(message)) = incoming.next().await {
        let header = message.header();
        if header.message_type() != Type::MethodCall {
            continue;
        }
        if let Some(answered) = remotes
            .answer(&bluez, &message, script.connect_error, &calls)
            .await
        {
            answered.unwrap();
            continue;
        }
        let path = header.path().map(ToString::to_string).unwrap_or_default();
        let member = header.member().map(ToString::to_string).unwrap_or_default();
        match (path.as_str(), member.as_str()) {
            ("/", "GetManagedObjects") => bluez.reply(&header, &objects(&script, &remotes)).await,
            (ADAPTER, "SetDiscoveryFilter") => {
                let filter: Properties = message.body().deserialize().unwrap();
                calls
                    .send(Call::SetDiscoveryFilter {
                        uuids: filter["UUIDs"].try_clone().unwrap().try_into().unwrap(),
                        transport: filter["Transport"].try_clone().unwrap().try_into().unwrap(),
                    })
                    .unwrap();
                bluez.reply(&header, &()).await
            }
            (ADAPTER, "StartDiscovery") => {
                calls.send(Call::StartDiscovery).unwrap();
                scanner.send(header.sender().unwrap().to_string()).unwrap();
                match script.start_error {
                    Some(error) => bluez.reply_error(&header, error, &"scripted").await,
                    None => bluez.reply(&header, &()).await,
                }
            }
            (ADAPTER, "StopDiscovery") => {
                calls.send(Call::StopDiscovery).unwrap();
                bluez.reply(&header, &()).await
            }
            // bluetoothd answers a registration only after reading the
            // objects back, which the registrant serves meanwhile.
            (ADAPTER, "RegisterApplication") => {
                let (bluez, calls, apps) = (bluez.clone(), calls.clone(), apps.clone());
                let refusal = script.register_error;
                let message = message.clone();
                tokio::spawn(async move {
                    let header = message.header();
                    let (root, _): (OwnedObjectPath, Properties) =
                        message.body().deserialize().unwrap();
                    let owner = header.sender().unwrap().to_string();
                    let (app, call) = central::read_application(&bluez, &owner, &root).await;
                    calls.send(call).unwrap();
                    apps.send(app).unwrap();
                    match refusal {
                        Some(error) => bluez.reply_error(&header, error, &"scripted").await,
                        None => bluez.reply(&header, &()).await,
                    }
                    .unwrap();
                });
                Ok(())
            }
            (ADAPTER, "RegisterAdvertisement") => {
                let (bluez, calls) = (bluez.clone(), calls.clone());
                let message = message.clone();
                tokio::spawn(async move {
                    let header = message.header();
                    let (path, _): (OwnedObjectPath, Properties) =
                        message.body().deserialize().unwrap();
                    let owner = header.sender().unwrap().to_string();
                    calls
                        .send(central::read_advertisement(&bluez, &owner, &path).await)
                        .unwrap();
                    bluez.reply(&header, &()).await.unwrap();
                });
                Ok(())
            }
            (device, "Get") if device.starts_with(&format!("{ADAPTER}/dev_")) => {
                let address = device[ADAPTER.len() + 5..].replace('_', ":");
                bluez.reply(&header, &Variant::from(address)).await
            }
            _ => {
                bluez
                    .reply_error(&header, "org.freedesktop.DBus.Error.UnknownMethod", &"")
                    .await
            }
        }
        .unwrap();
    }
}
