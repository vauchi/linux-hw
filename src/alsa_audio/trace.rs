// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use alsa::pcm::{Frames, PCM};
use std::cell::Cell;
use std::io::Write;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

pub(super) struct AudioTrace {
    call: u64,
    port: &'static str,
    started: Option<Instant>,
    last_progress: Cell<Option<Duration>>,
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
            last_progress: Cell::new(None),
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

    pub(super) fn configuration(&self, pcm: &PCM) {
        if self.started.is_none() {
            return;
        }
        let details = (|| -> Result<String, alsa::Error> {
            let hardware = pcm.hw_params_current()?;
            let software = pcm.sw_params_current()?;
            Ok(format!(
                "rate={} buffer_frames={} period_frames={} start_threshold={} stop_threshold={}",
                hardware.get_rate()?,
                hardware.get_buffer_size()?,
                hardware.get_period_size()?,
                software.get_start_threshold()?,
                software.get_stop_threshold()?
            ))
        })()
        .unwrap_or_else(|error| format!("query_error={error}"));
        let _ = writeln!(
            std::io::stderr(),
            "audio_configuration call={} port={} {details}",
            self.call,
            self.port
        );
    }

    pub(super) fn playback_progress(&self, pcm: &PCM, frames: Frames) {
        let Some(started) = self.started else { return };
        let elapsed = started.elapsed();
        if self
            .last_progress
            .get()
            .is_some_and(|last| elapsed - last < Duration::from_millis(250))
        {
            return;
        }
        self.last_progress.set(Some(elapsed));
        let _ = writeln!(
            std::io::stderr(),
            "audio_progress call={} port={} state={:?} delay_frames={frames} elapsed_ms={:.3}",
            self.call,
            self.port,
            pcm.state(),
            elapsed.as_secs_f64() * 1000.0
        );
    }

    pub(super) fn silent_tail(&self, frames: Frames) {
        if self.started.is_some() {
            let _ = writeln!(
                std::io::stderr(),
                "audio_tail call={} port={} trailing_silence_frames={frames}",
                self.call,
                self.port
            );
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
