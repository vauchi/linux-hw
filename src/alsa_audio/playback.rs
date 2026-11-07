// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use alsa::pcm::Frames;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Completion {
    Drained,
    Cancelled,
}

pub(super) trait PlaybackTail {
    fn start_if_needed(&mut self) -> Result<(), String>;
    fn refresh_progress(&mut self) -> Result<(), String>;
    fn delay_frames(&mut self) -> Result<Frames, String>;
    fn is_exhausted(&self) -> bool;
    fn wait(&mut self);
}

pub(super) fn wait_for_playback(
    playback: &mut impl PlaybackTail,
    cancelled: &AtomicBool,
    trailing_silence: Frames,
) -> Result<Completion, String> {
    if cancelled.load(Ordering::SeqCst) {
        return Ok(Completion::Cancelled);
    }
    playback.start_if_needed()?;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Completion::Cancelled);
        }
        playback.refresh_progress()?;
        if playback.is_exhausted() || playback.delay_frames()? <= trailing_silence {
            return Ok(Completion::Drained);
        }
        playback.wait();
    }
}

pub(super) fn write_samples(
    mut samples: &[f32],
    cancelled: &AtomicBool,
    mut write: impl FnMut(&[f32]) -> Result<usize, String>,
) -> Result<(), String> {
    while !samples.is_empty() && !cancelled.load(Ordering::SeqCst) {
        let chunk = &samples[..samples.len().min(super::CHUNK_FRAMES)];
        let written = write(chunk)?;
        if written == 0 {
            return Err("ALSA playback made no write progress".into());
        }
        samples = samples
            .get(written..)
            .filter(|_| written <= chunk.len())
            .ok_or("ALSA playback reported an invalid write count")?;
    }
    Ok(())
}
