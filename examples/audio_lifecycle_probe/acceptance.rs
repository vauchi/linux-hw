// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::trace::{Observed, Trace};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vauchi_contract::executor::InboxLimits;
use vauchi_linux_hw::{AudioBackend, AudioSession};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Completion,
    Duplex,
}

#[derive(Debug)]
pub struct Report {
    pub status: &'static str,
    pub active_at_observation: bool,
    pub natural_return: bool,
    pub return_ms: Option<f64>,
    pub unfinished_after_cleanup: usize,
    pub errors: Vec<String>,
}

/// Keep collecting after a clean inconclusive result, preserving the failed exit status.
pub fn run_modes(mut measure_mode: impl FnMut(Mode) -> Result<Report, String>) -> bool {
    let mut success = true;
    for mode in [Mode::Completion, Mode::Duplex] {
        let Ok(report) = measure_mode(mode) else {
            return false;
        };
        if report.unfinished_after_cleanup != 0 {
            return false;
        }
        success &= report.status == "measured" && report.errors.is_empty();
    }
    success
}

/// Measures backend call lifetimes; duplex readiness still requires native stage evidence.
pub fn measure(
    backend: Arc<dyn AudioBackend>,
    mode: Mode,
    settle: Duration,
    observation: Duration,
    on_observation: impl FnOnce(),
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
    let duplex = mode == Mode::Duplex;
    let count = if duplex { 2 } else { 1 };
    session
        .emit_challenge(vec![0.0; if duplex { 480_000 } else { 48_000 }], 48_000)
        .map_err(|error| error.to_string())?;
    if duplex {
        session
            .listen_for_response(10_000)
            .map_err(|error| error.to_string())?;
    }
    let cleanup = Duration::from_secs(3);
    trace.wait(cleanup, |calls| calls.len() == count);
    let active = trace.wait(settle, |calls| {
        calls.iter().any(|call| call.returned.is_some())
    });
    let active_at_observation =
        active.len() == count && active.iter().all(|call| call.returned.is_none());
    on_observation();
    let before_stop = if duplex {
        active
    } else {
        trace.wait(observation, |calls| {
            calls.iter().any(|call| call.returned.is_some())
        })
    };
    let natural_return = !duplex
        && active_at_observation
        && before_stop
            .first()
            .is_some_and(|call| call.returned.is_some() && !call.cancelled && call.error.is_none());
    eprintln!("audio_stop_requested mode={mode:?}");
    let stop = Instant::now();
    session.stop();
    let shutdown = runtime.block_on(session.shutdown());
    let calls = trace.wait(cleanup, |calls| {
        calls.len() == count && calls.iter().all(|call| call.returned.is_some())
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    let unfinished_after_cleanup =
        count - calls.iter().filter(|call| call.returned.is_some()).count();
    let mut errors: Vec<_> = calls.iter().filter_map(|call| call.error.clone()).collect();
    if let Err(error) = shutdown {
        errors.push(error.to_string());
    }
    let measured = active_at_observation
        && unfinished_after_cleanup == 0
        && errors.is_empty()
        && if duplex {
            calls.iter().all(|call| call.cancelled)
        } else {
            natural_return
        };
    let return_ms = if measured {
        let end = calls.iter().filter_map(|call| call.returned).max().unwrap();
        let begin = if duplex { stop } else { calls[0].entered };
        Some(end.saturating_duration_since(begin).as_secs_f64() * 1000.0)
    } else {
        None
    };
    Ok(Report {
        status: if measured { "measured" } else { "inconclusive" },
        active_at_observation,
        natural_return,
        return_ms,
        unfinished_after_cleanup,
        errors,
    })
}
