// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vauchi_linux_hw::AudioBackend;

#[derive(Clone)]
pub(crate) struct Call {
    pub entered: Instant,
    pub returned: Option<Instant>,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Trace {
    calls: Mutex<Vec<Call>>,
    changed: Condvar,
}

impl Trace {
    pub fn wait(&self, timeout: Duration, ready: impl Fn(&[Call]) -> bool) -> Vec<Call> {
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

pub(crate) struct Observed {
    pub backend: Arc<dyn AudioBackend>,
    pub trace: Arc<Trace>,
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
