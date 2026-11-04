// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `DirectSession` is the contract's direct-send executor (private#1029
//! 4d.7c): `DirectSend` runs one framed exchange with the peer at Core's
//! endpoint and reports the peer's payload as `DirectPayloadReceived`, or
//! the failure as a `HardwareError` on "USB", the transport GTK and Qt
//! have always named. Core chose the endpoint, role and deadlines; the
//! session decides nothing.
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vauchi_contract::executor::InboxLimits;
use vauchi_contract::{Command, DirectEndpoint, Event};
use vauchi_linux_hw::{DirectBackend, DirectSession, ExecuteError, HardwareShell};

struct ScriptedLink {
    seen: Mutex<Vec<(DirectEndpoint, Vec<u8>, bool)>>,
    answer: Result<Vec<u8>, String>,
}

impl DirectBackend for ScriptedLink {
    fn exchange(
        &self,
        endpoint: &DirectEndpoint,
        payload: &[u8],
        is_initiator: bool,
    ) -> Result<Vec<u8>, String> {
        self.seen
            .lock()
            .unwrap()
            .push((endpoint.clone(), payload.to_vec(), is_initiator));
        self.answer.clone()
    }
}

fn link(answer: Result<Vec<u8>, String>) -> Arc<ScriptedLink> {
    Arc::new(ScriptedLink {
        seen: Mutex::new(Vec::new()),
        answer,
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

fn session(backend: Arc<ScriptedLink>, runtime: &tokio::runtime::Runtime) -> DirectSession {
    let limits = InboxLimits {
        operations: NonZeroUsize::new(2).unwrap(),
        events: NonZeroUsize::new(16).unwrap(),
        event_bytes: NonZeroUsize::new(1 << 20).unwrap(),
    };
    DirectSession::with_backend(backend, runtime.handle().clone(), limits).unwrap()
}

fn endpoint() -> DirectEndpoint {
    DirectEndpoint {
        service_type: "_vauchi._tcp.local.".into(),
        port: 41_234,
        fallback_host: "192.168.42.129".into(),
        browse_timeout_ms: 2_000,
        io_timeout_ms: 5_000,
    }
}

fn next(pop: &mut dyn FnMut() -> Option<Event>) -> Option<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(event) = pop() {
            return Some(event);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

// @internal — Core's endpoint, payload and role reach the link unchanged.
#[test]
fn the_peers_payload_is_reported_as_received() {
    let runtime = runtime();
    let backend = link(Ok(b"theirs".to_vec()));
    let mut session = session(backend.clone(), &runtime);

    session.send(b"ours".to_vec(), true, endpoint()).unwrap();

    assert_eq!(
        next(&mut || session.pop()),
        Some(Event::DirectPayloadReceived {
            data: b"theirs".to_vec()
        })
    );
    assert_eq!(
        *backend.seen.lock().unwrap(),
        [(endpoint(), b"ours".to_vec(), true)]
    );
}

// @internal
#[test]
fn a_failed_exchange_is_a_hardware_error_on_usb() {
    let runtime = runtime();
    let mut session = session(link(Err("TCP connect failed".into())), &runtime);

    session.send(b"ours".to_vec(), false, endpoint()).unwrap();

    assert_eq!(
        next(&mut || session.pop()),
        Some(Event::HardwareError {
            transport: "USB".into(),
            error: "TCP connect failed".into(),
        })
    );
}

// @scenario: generic_presentation_protocol.feature :: Invalid boundary input fails safely
#[test]
fn an_empty_or_oversized_payload_never_reaches_the_link() {
    let runtime = runtime();
    let backend = link(Ok(b"theirs".to_vec()));
    let mut session = session(backend.clone(), &runtime);

    assert!(session.send(Vec::new(), true, endpoint()).is_err());
    assert!(session.send(vec![0; 65_537], true, endpoint()).is_err());
    assert!(backend.seen.lock().unwrap().is_empty());
}

// @internal — the shell hands DirectSend to the session, and has none without it.
#[test]
fn the_hardware_shell_routes_direct_send_to_its_session() {
    let runtime = runtime();
    let command = Command::DirectSend {
        payload: b"ours".to_vec(),
        is_initiator: true,
        endpoint: endpoint(),
    };
    assert_eq!(
        HardwareShell::new().execute(command.clone()),
        Err(ExecuteError::Unsupported)
    );

    let mut shell =
        HardwareShell::new().with_direct(session(link(Ok(b"theirs".to_vec())), &runtime));
    shell.execute(command).unwrap();
    assert_eq!(
        next(&mut || shell.pop()),
        Some(Event::DirectPayloadReceived {
            data: b"theirs".to_vec()
        })
    );
}
