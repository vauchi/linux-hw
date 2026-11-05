// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::playback::{Completion, PlaybackTail, wait_for_playback};
use alsa::pcm::Frames;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct QueuedAudio {
    pending: VecDeque<Result<Frames, String>>,
    remaining: Frames,
    started: bool,
    start_error: Option<String>,
    cancel_on_wait: bool,
    cancelled: Arc<AtomicBool>,
}

impl QueuedAudio {
    fn new(frames: &[Frames]) -> Self {
        Self {
            pending: frames.iter().copied().map(Ok).collect(),
            remaining: frames.first().copied().unwrap_or(0),
            started: false,
            start_error: None,
            cancel_on_wait: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn finish(&mut self) -> Result<Completion, String> {
        let cancelled = self.cancelled.clone();
        wait_for_playback(self, &cancelled)
    }
}

impl PlaybackTail for QueuedAudio {
    fn start_if_needed(&mut self) -> Result<(), String> {
        self.started = true;
        self.start_error.clone().map_or(Ok(()), Err)
    }

    fn pending_frames(&mut self) -> Result<Frames, String> {
        self.remaining = self.pending.pop_front().unwrap_or(Ok(0))?;
        Ok(self.remaining)
    }

    fn wait(&mut self) {
        if self.cancel_on_wait {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

// @internal
#[test]
fn cancellation_after_tail_wait_begins_returns_with_queued_frames_remaining() {
    let mut audio = QueuedAudio::new(&[480_000, 0]);
    audio.cancel_on_wait = true;
    assert_eq!(audio.finish(), Ok(Completion::Cancelled));
    assert_eq!(audio.remaining, 480_000);
}

// @internal
#[test]
fn ordinary_completion_waits_until_all_queued_frames_have_played() {
    let mut audio = QueuedAudio::new(&[480_000, 240_000, 0]);
    assert_eq!(audio.finish(), Ok(Completion::Drained));
    assert_eq!(audio.remaining, 0);
    assert!(audio.started);
}

// @internal
#[test]
fn an_already_cancelled_call_does_not_start_the_pending_stream() {
    let mut audio = QueuedAudio::new(&[48_000, 0]);
    audio.cancelled.store(true, Ordering::SeqCst);
    assert_eq!(audio.finish(), Ok(Completion::Cancelled));
    assert!(!audio.started);
    assert_eq!(audio.remaining, 48_000);
}

// @internal
#[test]
fn a_delay_query_failure_reaches_the_caller() {
    let mut audio = QueuedAudio::new(&[]);
    audio.pending.push_back(Err("delay query failed".into()));
    assert_eq!(audio.finish(), Err("delay query failed".into()));
}

// @internal
#[test]
fn a_start_failure_reaches_the_caller() {
    let mut audio = QueuedAudio::new(&[48_000]);
    audio.start_error = Some("stream start failed".into());
    assert_eq!(audio.finish(), Err("stream start failed".into()));
    assert_eq!(audio.remaining, 48_000);
}
