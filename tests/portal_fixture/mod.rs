// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use futures_util::StreamExt;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;
use zbus::{Connection, MessageStream, OwnedObjectPath, OwnedVariant, Variant};

const SERVICE: &str = "org.freedesktop.portal.Desktop";
const CAMERA: &str = "org.freedesktop.portal.Camera";

#[derive(Clone, Copy)]
pub enum Response {
    Grant,
    Deny,
    Malformed,
    Wait,
    HoldReply,
    Unavailable,
    NoCamera,
}

pub struct FakePortal {
    pub client: Connection,
    pub peers: Arc<Mutex<Vec<UnixStream>>>,
    pub accesses: Arc<AtomicUsize>,
    pub closes: Arc<AtomicUsize>,
    pub opened: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    bus: Child,
}

impl FakePortal {
    pub async fn start(response: Response) -> Self {
        let mut bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let service = zbus::connection::Builder::address(address.trim())
            .name(SERVICE)
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.trim())
            .build()
            .await
            .unwrap();
        let peers = Arc::new(Mutex::new(Vec::new()));
        let accesses = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let opened = Arc::new(Notify::new());
        let state = (
            peers.clone(),
            accesses.clone(),
            closes.clone(),
            opened.clone(),
        );
        let task = tokio::spawn(async move {
            let mut messages = MessageStream::from(&service);
            while let Some(Ok(message)) = messages.next().await {
                let header = message.header();
                if header.message_type() != zbus::message::Type::MethodCall {
                    continue;
                }
                let method = header.member().unwrap().as_str();
                match method {
                    "Get" => {
                        let (interface, property): (String, String) =
                            message.body().deserialize().unwrap();
                        assert_eq!(
                            (interface.as_str(), property.as_str()),
                            (CAMERA, "IsCameraPresent")
                        );
                        if matches!(response, Response::Unavailable) {
                            service
                                .reply_error(
                                    &header,
                                    "org.freedesktop.DBus.Error.UnknownInterface",
                                    &"no camera portal",
                                )
                                .await
                                .unwrap();
                        } else {
                            service
                                .reply(
                                    &header,
                                    &Variant::from(!matches!(response, Response::NoCamera)),
                                )
                                .await
                                .unwrap();
                        }
                    }
                    "AccessCamera" => {
                        state.1.fetch_add(1, Ordering::SeqCst);
                        if matches!(response, Response::HoldReply) {
                            state.3.notify_one();
                            continue;
                        }
                        let options: HashMap<String, OwnedVariant> =
                            message.body().deserialize().unwrap();
                        let token =
                            String::try_from(options["handle_token"].try_clone().unwrap()).unwrap();
                        let sender = header.sender().unwrap().as_str().to_owned();
                        let path = format!(
                            "/org/freedesktop/portal/desktop/request/{}/{}",
                            sender.trim_start_matches(':').replace('.', "_"),
                            token
                        );
                        let path = OwnedObjectPath::try_from(path).unwrap();
                        if matches!(response, Response::Malformed) {
                            service
                                .emit_signal(
                                    Some(sender.as_str()),
                                    path.as_str(),
                                    "org.freedesktop.portal.Request",
                                    "Response",
                                    &"invalid",
                                )
                                .await
                                .unwrap();
                        } else if !matches!(response, Response::Wait) {
                            let code = if matches!(response, Response::Grant) {
                                0u32
                            } else {
                                1u32
                            };
                            service
                                .emit_signal(
                                    Some(sender.as_str()),
                                    path.as_str(),
                                    "org.freedesktop.portal.Request",
                                    "Response",
                                    &(code, HashMap::<String, OwnedVariant>::new()),
                                )
                                .await
                                .unwrap();
                        }
                        service.reply(&header, &path).await.unwrap();
                        state.3.notify_one();
                    }
                    "OpenPipeWireRemote" => {
                        let (socket, peer) = UnixStream::pair().unwrap();
                        state.0.lock().unwrap().push(peer);
                        let fd = zbus::OwnedFd::from(std::os::fd::OwnedFd::from(socket));
                        service.reply(&header, &fd).await.unwrap();
                    }
                    "Close" => {
                        state.2.fetch_add(1, Ordering::SeqCst);
                        service.reply(&header, &()).await.unwrap();
                    }
                    _ => {
                        service
                            .reply_error(
                                &header,
                                "org.freedesktop.DBus.Error.UnknownMethod",
                                &method,
                            )
                            .await
                            .unwrap();
                    }
                }
            }
        });
        Self {
            client,
            peers,
            accesses,
            closes,
            opened,
            task,
            bus,
        }
    }
}

impl Drop for FakePortal {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}
