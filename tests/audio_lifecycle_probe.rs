// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#[path = "../examples/audio_lifecycle_probe/measurement.rs"]
mod measurement;

use measurement::{Port, measure};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use vauchi_linux_hw::AudioBackend;

#[derive(Default)]
struct DelayedExit(AtomicUsize);

impl DelayedExit {
    fn run(&self, cancelled: &AtomicBool) {
        let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        while !cancelled.load(Ordering::SeqCst) || (first && self.0.load(Ordering::SeqCst) < 2) {
            std::thread::yield_now();
        }
    }
}

impl AudioBackend for DelayedExit {
    fn emit(&self, _: &[f32], _: u32, cancelled: &AtomicBool) -> Result<(), String> {
        self.run(cancelled);
        Ok(())
    }

    fn record(&self, _: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        self.run(cancelled);
        Ok((Vec::new(), 48_000))
    }
}

// @internal — #770: the probe must detect overlap, not infer serialized disposal.
#[test]
fn delayed_native_exit_is_reported_as_overlapping_backend_calls() {
    for port in [Port::Emit, Port::Listen] {
        let report = measure(
            Arc::new(DelayedExit::default()),
            port,
            Duration::ZERO,
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(report.initial_active);
        assert_eq!(report.backend_overlap, Some(true));
        assert_eq!(report.peak_backend_calls, 2);
        assert!((0.0..2000.0).contains(&report.replacement_return_ms.unwrap()));
        assert_eq!(report.unfinished_after_cleanup, 0);
        assert!(report.errors.is_empty());
        assert_eq!(report.status, "measured");
    }
}

struct Refused;

impl AudioBackend for Refused {
    fn emit(&self, _: &[f32], _: u32, _: &AtomicBool) -> Result<(), String> {
        Err("device refused".into())
    }

    fn record(&self, _: Duration, _: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        Err("device refused".into())
    }
}

// @internal — #770: an unopened device cannot yield a cancellation measurement.
#[test]
fn a_native_error_is_inconclusive_instead_of_a_fast_cancellation_success() {
    for port in [Port::Emit, Port::Listen] {
        let report = measure(
            Arc::new(Refused),
            port,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(!report.initial_active);
        assert_eq!(report.backend_overlap, None);
        assert_eq!(report.peak_backend_calls, 1);
        assert_eq!(report.replacement_return_ms, None);
        assert_eq!(report.unfinished_after_cleanup, 0);
        assert_eq!(report.errors, ["device refused"]);
        assert_eq!(report.status, "inconclusive");
    }
}

#[derive(Default)]
struct CleanupOnly {
    calls: AtomicUsize,
    replacement_stopped: AtomicBool,
}

impl CleanupOnly {
    fn run(&self, cancelled: &AtomicBool) {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            while !self.replacement_stopped.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
        } else {
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            self.replacement_stopped.store(true, Ordering::SeqCst);
        }
    }
}

impl AudioBackend for CleanupOnly {
    fn emit(&self, _: &[f32], _: u32, cancelled: &AtomicBool) -> Result<(), String> {
        self.run(cancelled);
        Ok(())
    }

    fn record(&self, _: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        self.run(cancelled);
        Ok((Vec::new(), 48_000))
    }
}

// @internal — #770: cleanup cannot masquerade as replacement cancellation.
#[test]
fn an_old_call_returning_only_after_cleanup_is_not_a_successful_measurement() {
    let report = measure(
        Arc::new(CleanupOnly::default()),
        Port::Emit,
        Duration::ZERO,
        Duration::from_millis(50),
    )
    .unwrap();
    assert!(report.initial_active);
    assert_eq!(report.backend_overlap, Some(true));
    assert_eq!(report.unfinished_after_cleanup, 0);
    assert_eq!(report.status, "inconclusive");
}
