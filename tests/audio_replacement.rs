// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;
use vauchi_contract::executor::InboxLimits;
use vauchi_linux_hw::{AudioBackend, AudioSession};

const DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Port {
    Emit,
    Listen,
}

struct ControlledAudio {
    started: mpsc::Sender<Port>,
    stopped: mpsc::Sender<Port>,
    emissions: AtomicUsize,
    recordings: AtomicUsize,
}

impl ControlledAudio {
    fn run(&self, port: Port, cancelled: &AtomicBool) {
        let active = match port {
            Port::Emit => &self.emissions,
            Port::Listen => &self.recordings,
        };
        active.fetch_add(1, Ordering::SeqCst);
        self.started.send(port).unwrap();
        while !cancelled.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        active.fetch_sub(1, Ordering::SeqCst);
        self.stopped.send(port).unwrap();
    }
}

impl AudioBackend for ControlledAudio {
    fn emit(&self, _: &[f32], _: u32, cancelled: &AtomicBool) -> Result<(), String> {
        self.run(Port::Emit, cancelled);
        Ok(())
    }

    fn record(&self, _: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        self.run(Port::Listen, cancelled);
        Ok((vec![0.25], 48_000))
    }
}

fn start(session: &mut AudioSession, port: Port) {
    match port {
        Port::Emit => session.emit_challenge(vec![0.25], 48_000).unwrap(),
        Port::Listen => session.listen_for_response(60_000).unwrap(),
    }
}

fn replacement_stops_only_its_own_port(port: Port) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (started, starts) = mpsc::channel();
    let (stopped, stops) = mpsc::channel();
    let backend = Arc::new(ControlledAudio {
        started,
        stopped,
        emissions: AtomicUsize::new(0),
        recordings: AtomicUsize::new(0),
    });
    let limits = InboxLimits {
        operations: NonZeroUsize::new(2).unwrap(),
        events: NonZeroUsize::new(16).unwrap(),
        event_bytes: NonZeroUsize::new(4096).unwrap(),
    };
    let mut session =
        AudioSession::with_backend(backend.clone(), runtime.handle().clone(), limits).unwrap();

    start(&mut session, Port::Emit);
    assert_eq!(starts.recv_timeout(DEADLINE).unwrap(), Port::Emit);
    start(&mut session, Port::Listen);
    assert_eq!(starts.recv_timeout(DEADLINE).unwrap(), Port::Listen);
    start(&mut session, port);
    let replacement = starts.recv_timeout(DEADLINE);
    let cancelled = stops.recv_timeout(DEADLINE);
    let active = (
        backend.emissions.load(Ordering::SeqCst),
        backend.recordings.load(Ordering::SeqCst),
    );
    session.stop();
    runtime.block_on(session.shutdown()).unwrap();

    assert_eq!(replacement.unwrap(), port);
    assert_eq!(
        cancelled.unwrap(),
        port,
        "the replaced native operation stops"
    );
    assert_eq!(active, (1, 1), "the other audio port keeps running");
    assert_eq!(
        session.pop(),
        None,
        "cancelled audio produces no late event"
    );
}

// @internal — #770: replacing a command must release its native operation.
#[test]
fn replacing_an_emission_stops_the_old_emission_without_stopping_the_listen() {
    replacement_stops_only_its_own_port(Port::Emit);
}

// @internal — #770: recording and emission are independent native operations.
#[test]
fn replacing_a_listen_stops_the_old_recording_without_stopping_the_emission() {
    replacement_stops_only_its_own_port(Port::Listen);
}
