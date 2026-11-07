// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::playback::{Completion, PlaybackTail, finish_with_silence};
use alsa::pcm::Frames;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

struct Device<'a> {
    queued: Rc<Cell<Frames>>,
    fixed_latency: Frames,
    ticks: usize,
    cancelled: &'a AtomicBool,
}

impl PlaybackTail for Device<'_> {
    fn start_if_needed(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn refresh_progress(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn delay_frames(&mut self) -> Result<Frames, String> {
        Ok(self.queued.get() + self.fixed_latency)
    }
    fn is_exhausted(&self) -> bool {
        false
    }
    fn wait(&mut self) {
        self.queued.set((self.queued.get() - 1024).max(0));
        self.ticks += 1;
        if self.ticks == 64 {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

// @internal — #1029: a physical route can retain more latency than one chunk of silence.
#[test]
fn completion_waits_for_payload_on_a_device_with_large_fixed_latency() {
    let cancelled = AtomicBool::new(false);
    let queued = Rc::new(Cell::new(4096));
    let mut device = Device {
        queued: queued.clone(),
        fixed_latency: 8192,
        ticks: 0,
        cancelled: &cancelled,
    };
    let mut suffix = 0;
    let completion = finish_with_silence(&mut device, &cancelled, |frames| {
        suffix = frames;
        queued.set(queued.get() + frames);
        Ok(())
    });
    assert_eq!(completion, Ok(Completion::Drained));
    assert!(!cancelled.load(Ordering::SeqCst));
    assert!(suffix >= device.fixed_latency);
    assert!(queued.get() + device.fixed_latency <= suffix);
}

// @internal
#[test]
fn an_already_cancelled_tail_submits_no_additional_silence() {
    let cancelled = AtomicBool::new(true);
    let mut device = Device {
        queued: Rc::new(Cell::new(4096)),
        fixed_latency: 8192,
        ticks: 0,
        cancelled: &cancelled,
    };
    let completion = finish_with_silence(&mut device, &cancelled, |_| {
        panic!("cancelled playback must not append more samples")
    });
    assert_eq!(completion, Ok(Completion::Cancelled));
}

// @internal
#[test]
fn a_silence_write_failure_does_not_become_successful_completion() {
    let cancelled = AtomicBool::new(false);
    let mut device = Device {
        queued: Rc::new(Cell::new(4096)),
        fixed_latency: 8192,
        ticks: 0,
        cancelled: &cancelled,
    };
    assert_eq!(
        finish_with_silence(&mut device, &cancelled, |_| Err("write failed".into())),
        Err("write failed".into())
    );
}

// @internal — reject a broken latency report without allocating or submitting samples.
#[test]
fn overflowing_latency_fails_before_appending_silence() {
    let cancelled = AtomicBool::new(false);
    let mut device = Device {
        queued: Rc::new(Cell::new(Frames::MAX)),
        fixed_latency: 0,
        ticks: 0,
        cancelled: &cancelled,
    };
    assert_eq!(
        finish_with_silence(&mut device, &cancelled, |_| panic!(
            "overflow must not write"
        )),
        Err("ALSA playback latency exceeds the frame range".into())
    );
}
