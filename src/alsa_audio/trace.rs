// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use alsa::pcm::PCM;
use std::io::Write;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

pub(super) struct AudioTrace {
    call: u64,
    port: &'static str,
    started: Option<Instant>,
}

impl AudioTrace {
    pub(super) fn new(port: &'static str) -> Self {
        let enabled = std::env::var_os("VAUCHI_AUDIO_TRACE").is_some_and(|value| value == "1");
        Self {
            call: if enabled {
                NEXT_CALL.fetch_add(1, Ordering::Relaxed)
            } else {
                0
            },
            port,
            started: enabled.then(Instant::now),
        }
    }

    pub(super) fn stage<T>(&self, name: &'static str, operation: impl FnOnce() -> T) -> T {
        if self.started.is_none() {
            return operation();
        }
        self.event(name, "enter");
        let result = operation();
        self.event(name, "return");
        result
    }

    fn event(&self, stage: &str, edge: &str) {
        if let Some(started) = self.started {
            let _ = writeln!(
                std::io::stderr(),
                "audio_stage call={} port={} stage={stage} edge={edge} elapsed_ms={:.3}",
                self.call,
                self.port,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    pub(super) fn own(&self, pcm: PCM) -> TracedPcm<'_> {
        TracedPcm {
            pcm: Some(pcm),
            trace: self,
        }
    }
}

pub(super) struct TracedPcm<'a> {
    pcm: Option<PCM>,
    trace: &'a AudioTrace,
}

impl Deref for TracedPcm<'_> {
    type Target = PCM;

    fn deref(&self) -> &PCM {
        self.pcm.as_ref().expect("PCM remains owned until drop")
    }
}

impl Drop for TracedPcm<'_> {
    fn drop(&mut self) {
        self.trace.stage("close", || drop(self.pcm.take()));
    }
}
