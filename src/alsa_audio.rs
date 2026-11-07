// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ALSA devices behind the audio port (owner, 2026-11-02): the
//! `default` playback device plays Core's samples, the `default` capture
//! device records at the rate it negotiates. On PipeWire and PulseAudio
//! hosts `default` is their ALSA compatibility layer, so one small crate
//! reaches every desktop; a PipeWire-native port comes with the camera
//! preview's libpipewire dependency (private#1036).
use crate::ports::AudioBackend;
use alsa::pcm::{Access, Format, HwParams, PCM, State};
use alsa::{Direction, ValueOr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod playback;
mod trace;
use playback::{PlaybackTail, finish_with_silence, wait_for_playback, write_samples};
use trace::{AudioTrace, TracedPcm};

// INLINE_TEST_REQUIRED: tests/unit exercises this private native port without exporting it.
#[cfg(test)]
#[path = "../tests/unit/playback_tail.rs"]
mod playback_tail_tests;

// INLINE_TEST_REQUIRED: tests/unit covers the private ALSA boundary without exporting it.
#[cfg(test)]
#[path = "../tests/unit/playback_latency.rs"]
mod playback_latency_tests;

const DEVICE: &str = "default";
/// Frames handed to the device per write or read. Native calls, including
/// setup and cleanup, can block beyond a chunk's duration.
const CHUNK_FRAMES: usize = 1024;

pub struct AlsaAudio;

fn open(
    trace: &AudioTrace,
    direction: Direction,
    rate: u32,
) -> Result<(TracedPcm<'_>, u32), String> {
    let pcm = trace
        .stage("open", || PCM::new(DEVICE, direction, false))
        .map_err(|e| format!("ALSA open failed: {e}"))?;
    let pcm = trace.own(pcm);
    let negotiated = trace.stage("configure", || {
        let params = HwParams::any(&pcm).map_err(|e| format!("ALSA params: {e}"))?;
        params
            .set_channels(1)
            .map_err(|e| format!("ALSA channels: {e}"))?;
        params
            .set_rate_near(rate, ValueOr::Nearest)
            .map_err(|e| format!("ALSA rate: {e}"))?;
        params
            .set_format(Format::float())
            .map_err(|e| format!("ALSA format: {e}"))?;
        params
            .set_access(Access::RWInterleaved)
            .map_err(|e| format!("ALSA access: {e}"))?;
        if direction == Direction::Playback {
            // Unconstrained Pulse defaults can select a multi-second buffer.
            // A short emission then remains below the server's prebuffer.
            params
                .set_period_size_near(CHUNK_FRAMES as alsa::pcm::Frames, ValueOr::Nearest)
                .map_err(|error| format!("ALSA playback period: {error}"))?;
            params
                .set_buffer_size_near((4 * CHUNK_FRAMES) as alsa::pcm::Frames)
                .map_err(|error| format!("ALSA playback buffer: {error}"))?;
        }
        pcm.hw_params(&params)
            .map_err(|e| format!("ALSA hw_params: {e}"))?;
        params.get_rate().map_err(|e| format!("ALSA rate: {e}"))
    })?;
    trace.configuration(&pcm);
    Ok((pcm, negotiated))
}

impl AudioBackend for AlsaAudio {
    fn emit(
        &self,
        samples: &[f32],
        sample_rate: u32,
        cancelled: &AtomicBool,
    ) -> Result<(), String> {
        let trace = AudioTrace::new("Emit");
        trace.stage("operation", || {
            emit(samples, sample_rate, cancelled, &trace)
        })
    }

    fn record(&self, timeout: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        let trace = AudioTrace::new("Listen");
        trace.stage("operation", || record(timeout, cancelled, &trace))
    }
}

fn emit(
    samples: &[f32],
    sample_rate: u32,
    cancelled: &AtomicBool,
    trace: &AudioTrace,
) -> Result<(), String> {
    let (pcm, rate) = open(trace, Direction::Playback, sample_rate)?;
    // The device may not offer Core's rate; the samples play at the
    // nearest one the device negotiated, as cpal's path did.
    let _ = rate;
    let io = trace
        .stage("io", || pcm.io_f32())
        .map_err(|e| format!("ALSA io: {e}"))?;
    let mut write = |chunk: &[f32]| {
        match io.writei(chunk) {
            Ok(written) => Ok(written),
            Err(e) => {
                // An underrun is recovered and the chunk retried once.
                trace
                    .stage("recover", || pcm.try_recover(e, true))
                    .map_err(|e| format!("ALSA play error: {e}"))?;
                trace
                    .stage("retry_write", || io.writei(chunk))
                    .map_err(|e| format!("ALSA play error: {e}"))
            }
        }
    };
    trace.stage("write", || write_samples(samples, cancelled, &mut write))?;
    let finished = trace.stage("finish", || {
        let mut tail = PcmTail(&pcm, trace);
        if samples.is_empty() {
            return wait_for_playback(&mut tail, cancelled, 0);
        }
        finish_with_silence(&mut tail, cancelled, |frames| {
            trace.silent_tail(frames);
            trace.stage("padding", || {
                let mut remaining = frames;
                while remaining > 0 && !cancelled.load(Ordering::SeqCst) {
                    let count = remaining.min(CHUNK_FRAMES as alsa::pcm::Frames) as usize;
                    write_samples(&[0.0; CHUNK_FRAMES][..count], cancelled, &mut write)?;
                    remaining -= count as alsa::pcm::Frames;
                }
                Ok(())
            })
        })
    });
    let stopped = trace
        .stage("drop", || PCM::drop(&pcm))
        .map_err(|error| format!("ALSA playback stop: {error}"));
    finished.and(stopped)
}

struct PcmTail<'a>(&'a PCM, &'a AudioTrace);

impl PlaybackTail for PcmTail<'_> {
    fn start_if_needed(&mut self) -> Result<(), String> {
        if self.0.state() == State::Prepared {
            self.0
                .start()
                .map_err(|error| format!("ALSA playback start: {error}"))?;
        }
        Ok(())
    }

    fn refresh_progress(&mut self) -> Result<(), String> {
        // Plugin state can be cached until its hardware pointer is refreshed.
        match self.0.avail_update() {
            Ok(_) => Ok(()),
            Err(error)
                if std::io::Error::from_raw_os_error(error.errno()).kind()
                    == std::io::ErrorKind::BrokenPipe =>
            {
                Ok(())
            }
            Err(error) => Err(format!("ALSA playback progress: {error}")),
        }
    }

    fn delay_frames(&mut self) -> Result<alsa::pcm::Frames, String> {
        let frames = match self.0.delay() {
            Ok(frames) => frames,
            Err(error)
                if std::io::Error::from_raw_os_error(error.errno()).kind()
                    == std::io::ErrorKind::BrokenPipe =>
            {
                0
            }
            Err(error) => return Err(format!("ALSA playback delay: {error}")),
        };
        self.1.playback_progress(self.0, frames);
        Ok(frames)
    }

    fn is_exhausted(&self) -> bool {
        self.0.state() == State::XRun
    }

    fn wait(&mut self) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn record(
    timeout: Duration,
    cancelled: &AtomicBool,
    trace: &AudioTrace,
) -> Result<(Vec<f32>, u32), String> {
    let (pcm, rate) = open(trace, Direction::Capture, 48_000)?;
    let io = trace
        .stage("io", || pcm.io_f32())
        .map_err(|e| format!("ALSA io: {e}"))?;
    trace
        .stage("start", || pcm.start())
        .map_err(|e| format!("ALSA record error: {e}"))?;
    let deadline = Instant::now() + timeout;
    let mut recorded = Vec::new();
    let mut buffer = vec![0f32; CHUNK_FRAMES];
    trace.stage("read", || {
        while Instant::now() < deadline && !cancelled.load(Ordering::SeqCst) {
            match io.readi(&mut buffer) {
                Ok(frames) => recorded.extend_from_slice(&buffer[..frames]),
                Err(e) => {
                    trace
                        .stage("recover", || pcm.try_recover(e, true))
                        .map_err(|e| format!("ALSA record error: {e}"))?;
                }
            }
        }
        Ok::<(), String>(())
    })?;
    Ok((recorded, rate))
}
