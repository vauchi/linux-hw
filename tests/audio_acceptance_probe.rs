// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#[path = "../examples/audio_lifecycle_probe/acceptance.rs"]
mod acceptance;
#[path = "../examples/audio_lifecycle_probe/trace.rs"]
mod trace;

use acceptance::{Mode, measure};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use vauchi_linux_hw::AudioBackend;

#[derive(Default)]
struct Natural {
    released: Mutex<bool>,
    changed: Condvar,
}

impl Natural {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

impl AudioBackend for Natural {
    fn emit(&self, _: &[f32], _: u32, _: &AtomicBool) -> Result<(), String> {
        let guard = self.released.lock().unwrap();
        let (released, _) = self
            .changed
            .wait_timeout_while(guard, Duration::from_secs(2), |released| !*released)
            .unwrap();
        if *released {
            Ok(())
        } else {
            Err("test gate timed out".into())
        }
    }

    fn record(&self, _: Duration, _: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        Err("unexpected capture".into())
    }
}

// @internal — #1029: normal completion must happen without a cancellation request.
#[test]
fn natural_completion_is_measured_before_stop() {
    let backend = Arc::new(Natural::default());
    let report = measure(
        backend.clone(),
        Mode::Completion,
        Duration::ZERO,
        Duration::from_secs(2),
        || backend.release(),
    )
    .unwrap();
    assert_eq!(report.status, "measured");
    assert!(report.active_at_observation);
    assert!(report.natural_return);
    assert_eq!(report.unfinished_after_cleanup, 0);
    assert_eq!(report.errors, Vec::<String>::new());
    assert!(report.return_ms.is_some());
}

struct UntilCancelled;

impl UntilCancelled {
    fn wait(&self, cancelled: &AtomicBool) {
        while !cancelled.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
    }
}

impl AudioBackend for UntilCancelled {
    fn emit(&self, _: &[f32], _: u32, cancelled: &AtomicBool) -> Result<(), String> {
        self.wait(cancelled);
        Ok(())
    }

    fn record(&self, _: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        self.wait(cancelled);
        Ok((Vec::new(), 48_000))
    }
}

// @internal — #1029: a return caused by cleanup is not natural completion.
#[test]
fn cleanup_only_return_does_not_pass_completion() {
    let report = measure(
        Arc::new(UntilCancelled),
        Mode::Completion,
        Duration::ZERO,
        Duration::ZERO,
        || {},
    )
    .unwrap();
    assert_eq!(report.status, "inconclusive");
    assert!(!report.natural_return);
    assert_eq!(report.return_ms, None);
}

// @internal — #1029: concurrent calls must both survive until the stop request.
#[test]
fn concurrent_calls_are_measured_and_cleaned_up() {
    let report = measure(
        Arc::new(UntilCancelled),
        Mode::Duplex,
        Duration::ZERO,
        Duration::from_secs(2),
        || {},
    )
    .unwrap();
    assert_eq!(report.status, "measured");
    assert!(report.active_at_observation);
    assert!(!report.natural_return);
    assert_eq!(report.unfinished_after_cleanup, 0);
    assert_eq!(report.errors, Vec::<String>::new());
    assert!(report.return_ms.is_some());
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

// @internal — #1029: refused devices must not look like fast completion or cleanup.
#[test]
fn refused_devices_are_inconclusive_in_both_modes() {
    for mode in [Mode::Completion, Mode::Duplex] {
        let report = measure(
            Arc::new(Refused),
            mode,
            Duration::from_secs(2),
            Duration::from_secs(2),
            || {},
        )
        .unwrap();
        assert_eq!(report.status, "inconclusive");
        assert!(!report.active_at_observation);
        assert!(!report.natural_return);
        assert_eq!(report.return_ms, None);
        assert_eq!(report.unfinished_after_cleanup, 0);
        assert_eq!(
            report.errors.len(),
            if mode == Mode::Completion { 1 } else { 2 }
        );
        assert!(report.errors.iter().all(|error| error == "device refused"));
    }
}

struct Immediate;

impl AudioBackend for Immediate {
    fn emit(&self, _: &[f32], _: u32, _: &AtomicBool) -> Result<(), String> {
        Ok(())
    }

    fn record(&self, _: Duration, _: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        Ok((Vec::new(), 48_000))
    }
}

// @internal — #1029: immediate successful returns establish no sustained activity.
#[test]
fn immediate_success_is_not_sustained_completion_or_duplex_activity() {
    for mode in [Mode::Completion, Mode::Duplex] {
        let report = measure(
            Arc::new(Immediate),
            mode,
            Duration::from_secs(2),
            Duration::from_secs(2),
            || {},
        )
        .unwrap();
        assert_eq!(report.status, "inconclusive");
        assert!(!report.active_at_observation);
        assert!(!report.natural_return);
        assert_eq!(report.return_ms, None);
        assert_eq!(report.unfinished_after_cleanup, 0);
        assert_eq!(report.errors, Vec::<String>::new());
    }
}

fn run_scripted_modes(status: &'static str, unfinished: usize) -> (bool, Vec<Mode>) {
    let mut modes = Vec::new();
    let success = acceptance::run_modes(|mode| {
        modes.push(mode);
        Ok(acceptance::Report {
            status,
            active_at_observation: true,
            natural_return: mode == Mode::Completion,
            return_ms: Some(1.0),
            unfinished_after_cleanup: unfinished,
            errors: Vec::new(),
        })
    });
    (success, modes)
}

// @internal — successful measurements must include both acceptance modes.
#[test]
fn both_successful_modes_pass_the_complete_probe() {
    assert_eq!(
        run_scripted_modes("measured", 0),
        (true, vec![Mode::Completion, Mode::Duplex])
    );
}

// @internal — collect independent evidence after clean failure without hiding it.
#[test]
fn inconclusive_completion_with_clean_shutdown_still_measures_duplex_and_fails() {
    assert_eq!(
        run_scripted_modes("inconclusive", 0),
        (false, vec![Mode::Completion, Mode::Duplex])
    );
}

// @internal — avoid starting more native work when the previous worker did not finish.
#[test]
fn unfinished_cleanup_prevents_the_next_measurement() {
    assert_eq!(
        run_scripted_modes("inconclusive", 1),
        (false, vec![Mode::Completion])
    );
}

// @internal
#[test]
fn measurement_error_prevents_the_next_measurement() {
    let mut modes = Vec::new();
    assert!(!acceptance::run_modes(|mode| {
        modes.push(mode);
        Err("runtime failed".into())
    }));
    assert_eq!(modes, vec![Mode::Completion]);
}
