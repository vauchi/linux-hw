// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

mod access;

use futures_util::StreamExt;
use std::{
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;
use zbus::{Connection, MessageStream, Variant};

const SERVICE: &str = "org.freedesktop.portal.Desktop";
const CAMERA: &str = "org.freedesktop.portal.Camera";

#[derive(Clone, Copy)]
pub enum Response {
    Grant,
    NativeGrant,
    Deny,
    Malformed,
    Wait,
    HoldReply,
    Unavailable,
    NoCamera,
}

pub struct FakePortal {
    pub client: Connection,
    pub server: Connection,
    pub peers: Arc<Mutex<Vec<UnixStream>>>,
    pub accesses: Arc<AtomicUsize>,
    pub closes: Arc<AtomicUsize>,
    pub opened: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    bus: Child,
}

impl FakePortal {
    pub async fn start(response: Response) -> Self {
        Self::start_inner(response, None).await
    }

    pub async fn start_paused(response: Response) -> (Self, tokio::sync::oneshot::Sender<()>) {
        let (start, gate) = tokio::sync::oneshot::channel();
        (Self::start_inner(response, Some(gate)).await, start)
    }

    async fn start_inner(
        response: Response,
        gate: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> Self {
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
        let server = service.clone();
        let mut messages = MessageStream::from(&service);
        let task = tokio::spawn(async move {
            if let Some(gate) = gate {
                gate.await.unwrap();
            }
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
                        access::respond(&service, &message, response, &state.1, &state.3).await;
                    }
                    "OpenPipeWireRemote" => {
                        let socket = if matches!(response, Response::NativeGrant) {
                            UnixStream::connect(
                                std::env::var("VAUCHI_TEST_PIPEWIRE_SOCKET").unwrap(),
                            )
                            .unwrap()
                        } else {
                            let (socket, peer) = UnixStream::pair().unwrap();
                            state.0.lock().unwrap().push(peer);
                            socket
                        };
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
            server,
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
