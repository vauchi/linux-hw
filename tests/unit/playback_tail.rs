// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::playback::{Completion, PlaybackTail, wait_for_playback, write_samples};
use alsa::pcm::Frames;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct QueuedAudio {
    pending: VecDeque<Result<Frames, String>>,
    remaining: Frames,
    started: bool,
    exhausted: bool,
    start_error: Option<String>,
    refresh_error: Option<String>,
    exhausted_on_refresh: bool,
    cancel_on_wait: bool,
    cancelled: Arc<AtomicBool>,
}

impl QueuedAudio {
    fn new(frames: &[Frames]) -> Self {
        Self {
            pending: frames.iter().copied().map(Ok).collect(),
            remaining: frames.first().copied().unwrap_or(0),
            started: false,
            exhausted: false,
            start_error: None,
            refresh_error: None,
            exhausted_on_refresh: false,
            cancel_on_wait: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn finish(&mut self) -> Result<Completion, String> {
        let cancelled = self.cancelled.clone();
        wait_for_playback(self, &cancelled, 0)
    }
}

impl PlaybackTail for QueuedAudio {
    fn start_if_needed(&mut self) -> Result<(), String> {
        self.started = true;
        self.start_error.clone().map_or(Ok(()), Err)
    }

    fn delay_frames(&mut self) -> Result<Frames, String> {
        self.remaining = self.pending.pop_front().unwrap_or(Ok(0))?;
        Ok(self.remaining)
    }

    fn refresh_progress(&mut self) -> Result<(), String> {
        if self.exhausted_on_refresh {
            self.exhausted = true;
        }
        self.refresh_error.clone().map_or(Ok(()), Err)
    }

    fn is_exhausted(&self) -> bool {
        self.exhausted
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
fn an_exhausted_stream_completes_even_when_fixed_latency_stays_positive() {
    let mut audio = QueuedAudio::new(&[2_400, 0]);
    audio.exhausted = true;
    audio.cancel_on_wait = true;
    assert_eq!(audio.finish(), Ok(Completion::Drained));
    assert_eq!(audio.remaining, 2_400);
}

// @internal
#[test]
fn an_already_cancelled_call_does_not_start_the_pending_stream() {
    let mut audio = QueuedAudio::new(&[48_000, 0]);
    audio.cancelled.store(true, Ordering::SeqCst);
    audio.refresh_error = Some("cancelled calls must not query progress".into());
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

// @internal — plugin state may remain cached while delay includes fixed device latency.
#[test]
fn fresh_progress_completes_an_exhausted_stream_with_positive_latency() {
    let mut audio = QueuedAudio::new(&[2_400]);
    audio.exhausted_on_refresh = true;
    audio.cancel_on_wait = true;
    assert_eq!(audio.finish(), Ok(Completion::Drained));
    assert_eq!(audio.remaining, 2_400);
}

// @internal
#[test]
fn a_progress_refresh_failure_reaches_the_caller() {
    let mut audio = QueuedAudio::new(&[0]);
    audio.refresh_error = Some("progress query failed".into());
    assert_eq!(audio.finish(), Err("progress query failed".into()));
}

// @internal — a completed stream need not support another delay query.
#[test]
fn fresh_exhaustion_completes_before_a_failing_delay_query() {
    let mut audio = QueuedAudio::new(&[]);
    audio
        .pending
        .push_back(Err("delay unavailable after exhaustion".into()));
    audio.exhausted_on_refresh = true;
    assert_eq!(audio.finish(), Ok(Completion::Drained));
}

// @internal — a positive fixed device latency must not hide real-sample completion.
#[test]
fn trailing_silence_allows_completion_without_waiting_for_zero_delay() {
    let mut audio = QueuedAudio::new(&[2_048, 1_025, 1_024, 133]);
    let cancelled = audio.cancelled.clone();
    assert_eq!(
        wait_for_playback(&mut audio, &cancelled, 1_024),
        Ok(Completion::Drained)
    );
    assert_eq!(audio.remaining, 1_024);
}

// @internal — silence queued after the payload does not make cancellation successful completion.
#[test]
fn cancellation_still_interrupts_a_tail_containing_silence() {
    let mut audio = QueuedAudio::new(&[2_048, 1_024]);
    audio.cancel_on_wait = true;
    let cancelled = audio.cancelled.clone();
    assert_eq!(
        wait_for_playback(&mut audio, &cancelled, 1_024),
        Ok(Completion::Cancelled)
    );
    assert_eq!(audio.remaining, 2_048);
}

// @internal — completion accounting requires every submitted payload frame to be queued.
#[test]
fn partial_writes_preserve_every_payload_frame_in_order() {
    let samples: Vec<f32> = (0..2_500).map(|value| value as f32).collect();
    let mut written = Vec::new();
    write_samples(&samples, &AtomicBool::new(false), |chunk| {
        let count = chunk.len().min(137);
        written.extend_from_slice(&chunk[..count]);
        Ok(count)
    })
    .unwrap();
    assert_eq!(written, samples);
}

// @internal
#[test]
fn a_zero_frame_write_fails_instead_of_spinning_or_losing_samples() {
    assert_eq!(
        write_samples(&[1.0], &AtomicBool::new(false), |_| Ok(0)),
        Err("ALSA playback made no write progress".into())
    );
}

// @internal
#[test]
fn cancellation_between_partial_writes_leaves_the_remainder_unsubmitted() {
    let cancelled = AtomicBool::new(false);
    let mut written = Vec::new();
    write_samples(&[1.0, 2.0], &cancelled, |chunk| {
        written.push(chunk[0]);
        cancelled.store(true, Ordering::SeqCst);
        Ok(1)
    })
    .unwrap();
    assert_eq!(written, vec![1.0]);
}

// @internal
#[test]
fn write_failure_reaches_the_caller_without_a_successful_tail() {
    assert_eq!(
        write_samples(&[1.0], &AtomicBool::new(false), |_| Err(
            "device failed".into()
        )),
        Err("device failed".into())
    );
}
