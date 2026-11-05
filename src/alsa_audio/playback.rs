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
    fn delay_frames(&mut self) -> Result<Frames, String>;
    fn is_exhausted(&self) -> bool;
    fn wait(&mut self);
}

pub(super) fn wait_for_playback(
    playback: &mut impl PlaybackTail,
    cancelled: &AtomicBool,
) -> Result<Completion, String> {
    if cancelled.load(Ordering::SeqCst) {
        return Ok(Completion::Cancelled);
    }
    playback.start_if_needed()?;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Completion::Cancelled);
        }
        if playback.delay_frames()? <= 0 || playback.is_exhausted() {
            return Ok(Completion::Drained);
        }
        playback.wait();
    }
}
