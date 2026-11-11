// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AudioSession` is the contract's audio executor: `AudioEmitChallenge`
//! plays Core's pre-encoded samples, `AudioListenForResponse` records for
//! the given time and reports `AudioSamplesRecorded`, `AudioStop` ends
//! both. Emitting and listening run independently, as a mutual exchange
//! does both at once. A failure is a `HardwareError` on "Audio".
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vauchi_contract::Event;
use vauchi_contract::executor::InboxLimits;
use vauchi_linux_hw::{AudioBackend, AudioSession};

#[derive(Default)]
struct ScriptedAudio {
    emitted: Mutex<Vec<(Vec<f32>, u32)>>,
    emit_fails: Option<String>,
    recorded: Option<(Vec<f32>, u32)>,
    record_fails: Option<String>,
    /// Emitting blocks until this is set, so a test can overlap a listen.
    emit_gate: Arc<AtomicBool>,
    records: AtomicUsize,
}

impl AudioBackend for ScriptedAudio {
    fn emit(
        &self,
        samples: &[f32],
        sample_rate: u32,
        cancelled: &AtomicBool,
    ) -> Result<(), String> {
        self.emitted
            .lock()
            .unwrap()
            .push((samples.to_vec(), sample_rate));
        while !self.emit_gate.load(Ordering::SeqCst) && !cancelled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        match &self.emit_fails {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    fn record(&self, timeout: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        self.records.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && !cancelled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err("audio stopped".into());
        }
        match (&self.record_fails, &self.recorded) {
            (Some(reason), _) => Err(reason.clone()),
            (None, Some(recorded)) => Ok(recorded.clone()),
            (None, None) => Ok((Vec::new(), 48_000)),
        }
    }
}

fn session(backend: Arc<ScriptedAudio>, runtime: &tokio::runtime::Runtime) -> AudioSession {
    let limits = InboxLimits {
        operations: NonZeroUsize::new(2).unwrap(),
        events: NonZeroUsize::new(16).unwrap(),
        event_bytes: NonZeroUsize::new(1 << 20).unwrap(),
    };
    AudioSession::with_backend(backend, runtime.handle().clone(), limits).unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

fn drain(session: &mut AudioSession, expected: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    while events.len() < expected && Instant::now() < deadline {
        match session.pop() {
            Some(event) => events.push(event),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    events
}

fn audio_error(error: &str) -> Event {
    Event::HardwareError {
        transport: "Audio".into(),
        error: error.into(),
    }
}

// @internal
#[test]
fn emit_plays_cores_samples_verbatim_and_reports_nothing_on_success() {
    let runtime = runtime();
    let backend = Arc::new(ScriptedAudio {
        emit_gate: Arc::new(AtomicBool::new(true)),
        ..Default::default()
    });
    let mut session = session(backend.clone(), &runtime);
    session
        .emit_challenge(vec![0.1, -0.1, 0.5], 44_100)
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        backend.emitted.lock().unwrap().clone(),
        vec![(vec![0.1, -0.1, 0.5], 44_100)]
    );
    assert_eq!(session.pop(), None);
}

// @internal
#[test]
fn listen_records_for_the_given_time_and_reports_the_samples_at_the_devices_rate() {
    let runtime = runtime();
    let backend = Arc::new(ScriptedAudio {
        recorded: Some((vec![0.25, 0.5], 48_000)),
        ..Default::default()
    });
    let mut session = session(backend, &runtime);
    session.listen_for_response(20).unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![Event::AudioSamplesRecorded {
            samples: vec![0.25, 0.5],
            sample_rate: 48_000,
        }]
    );
}

// @internal
#[test]
fn a_backend_that_cannot_play_or_record_reports_a_hardware_error_on_audio() {
    let runtime = runtime();
    let backend = Arc::new(ScriptedAudio {
        emit_fails: Some("No output device".into()),
        record_fails: Some("No input device".into()),
        emit_gate: Arc::new(AtomicBool::new(true)),
        ..Default::default()
    });
    let mut session = session(backend, &runtime);
    session.emit_challenge(vec![0.0], 44_100).unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![audio_error("No output device")]
    );
    session.listen_for_response(10).unwrap();
    assert_eq!(drain(&mut session, 1), vec![audio_error("No input device")]);
}

// @scenario: contact_exchange.feature :: In a mutual QR exchange one side emits the audio challenge first
#[test]
fn listening_does_not_interrupt_an_emission_in_progress() {
    let runtime = runtime();
    let gate = Arc::new(AtomicBool::new(false));
    let backend = Arc::new(ScriptedAudio {
        recorded: Some((vec![1.0], 48_000)),
        emit_gate: gate.clone(),
        ..Default::default()
    });
    let mut session = session(backend.clone(), &runtime);
    session.emit_challenge(vec![0.3], 44_100).unwrap();
    session.listen_for_response(10).unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![Event::AudioSamplesRecorded {
            samples: vec![1.0],
            sample_rate: 48_000,
        }]
    );
    assert_eq!(
        backend.emitted.lock().unwrap().len(),
        1,
        "the emission kept running"
    );
    gate.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(session.pop(), None);
}

// @internal
#[test]
fn stop_ends_a_listen_without_a_report_core_no_longer_expects() {
    let runtime = runtime();
    let backend = Arc::new(ScriptedAudio {
        recorded: Some((vec![1.0], 48_000)),
        ..Default::default()
    });
    let mut session = session(backend.clone(), &runtime);
    session.listen_for_response(5_000).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    session.stop();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(backend.records.load(Ordering::SeqCst), 1);
    assert_eq!(session.pop(), None);
}
