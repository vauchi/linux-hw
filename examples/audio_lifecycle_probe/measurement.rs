// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vauchi_contract::executor::InboxLimits;
use vauchi_linux_hw::{AudioBackend, AudioSession};

#[derive(Clone, Copy, Debug)]
pub enum Port {
    Emit,
    Listen,
}

#[derive(Debug)]
pub struct Report {
    pub initial_active: bool,
    pub replacement_return_ms: Option<f64>,
    pub backend_overlap: Option<bool>,
    pub peak_backend_calls: usize,
    pub unfinished_after_cleanup: usize,
    pub errors: Vec<String>,
    pub status: &'static str,
}

#[derive(Clone)]
struct Call {
    entered: Instant,
    returned: Option<Instant>,
    cancelled: bool,
    error: Option<String>,
}

#[derive(Default)]
struct Trace {
    calls: Mutex<Vec<Call>>,
    changed: Condvar,
}

impl Trace {
    fn wait(&self, timeout: Duration, ready: impl Fn(&[Call]) -> bool) -> Vec<Call> {
        let calls = self.calls.lock().unwrap();
        let (calls, _) = self
            .changed
            .wait_timeout_while(calls, timeout, |calls| !ready(calls))
            .unwrap();
        calls.clone()
    }

    fn invoke<T>(
        &self,
        cancelled: &AtomicBool,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let id = {
            let mut calls = self.calls.lock().unwrap();
            let id = calls.len();
            calls.push(Call {
                entered: Instant::now(),
                returned: None,
                cancelled: false,
                error: None,
            });
            self.changed.notify_all();
            id
        };
        let result = operation();
        let mut calls = self.calls.lock().unwrap();
        calls[id].returned = Some(Instant::now());
        calls[id].cancelled = cancelled.load(Ordering::SeqCst);
        calls[id].error = result.as_ref().err().cloned();
        self.changed.notify_all();
        result
    }
}

struct Observed {
    backend: Arc<dyn AudioBackend>,
    trace: Arc<Trace>,
}

impl AudioBackend for Observed {
    fn emit(&self, samples: &[f32], rate: u32, cancelled: &AtomicBool) -> Result<(), String> {
        self.trace
            .invoke(cancelled, || self.backend.emit(samples, rate, cancelled))
    }

    fn record(&self, timeout: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        self.trace
            .invoke(cancelled, || self.backend.record(timeout, cancelled))
    }
}

fn start(session: &mut AudioSession, port: Port) {
    match port {
        Port::Emit => session.emit_challenge(vec![0.0; 480_000], 48_000),
        Port::Listen => session.listen_for_response(10_000),
    }
    .expect("probe requests stay within the audio API bounds");
}

/// Measure API call intervals, not physical PCM ownership or acoustic output.
pub fn measure(
    backend: Arc<dyn AudioBackend>,
    port: Port,
    settle: Duration,
    deadline: Duration,
) -> Result<Report, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let trace = Arc::new(Trace::default());
    let observed = Arc::new(Observed {
        backend,
        trace: trace.clone(),
    });
    let limits = InboxLimits {
        operations: NonZeroUsize::new(2).unwrap(),
        events: NonZeroUsize::new(16).unwrap(),
        event_bytes: NonZeroUsize::new(4 << 20).unwrap(),
    };
    let mut session = AudioSession::with_backend(observed, runtime.handle().clone(), limits)
        .map_err(|error| error.to_string())?;
    start(&mut session, port);
    trace.wait(deadline, |calls| !calls.is_empty());
    let before = trace.wait(settle, |calls| {
        calls.first().is_some_and(|call| call.returned.is_some())
    });
    let initial_active = before.first().is_some_and(|call| call.returned.is_none());
    let replacement = Instant::now();
    if initial_active {
        start(&mut session, port);
        trace.wait(deadline, |calls| {
            calls.len() == 2 && calls[0].returned.is_some()
        });
    }
    session.stop();
    let shutdown = runtime.block_on(session.shutdown());
    let calls = trace.wait(deadline, |calls| {
        !calls.is_empty() && calls.iter().all(|call| call.returned.is_some())
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    let first_return = calls.first().and_then(|call| call.returned);
    let replacement_return_ms = if initial_active {
        first_return.map(|time| time.saturating_duration_since(replacement).as_secs_f64() * 1000.0)
    } else {
        None
    };
    let backend_overlap = calls
        .get(1)
        .map(|second| first_return.is_none_or(|returned| second.entered < returned));
    let peak_backend_calls = calls
        .iter()
        .map(|at| {
            calls
                .iter()
                .filter(|call| {
                    call.entered <= at.entered
                        && call.returned.is_none_or(|returned| returned > at.entered)
                })
                .count()
        })
        .max()
        .unwrap_or(0);
    let unfinished_after_cleanup = calls.iter().filter(|call| call.returned.is_none()).count();
    let mut errors: Vec<_> = calls.iter().filter_map(|call| call.error.clone()).collect();
    if let Err(error) = shutdown {
        errors.push(error.to_string());
    }
    let status = if initial_active
        && calls.len() == 2
        && calls[0].cancelled
        && unfinished_after_cleanup == 0
        && errors.is_empty()
    {
        "measured"
    } else {
        "inconclusive"
    };
    Ok(Report {
        initial_active,
        replacement_return_ms,
        backend_overlap,
        peak_backend_calls,
        unfinished_after_cleanup,
        errors,
        status,
    })
}
