// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ALSA devices behind the audio port (owner, 2026-11-02): the
//! `default` playback device plays Core's samples, the `default` capture
//! device records at the rate it negotiates. On PipeWire and PulseAudio
//! hosts `default` is their ALSA compatibility layer, so one small crate
//! reaches every desktop; a PipeWire-native port comes with the camera
//! preview's libpipewire dependency (private#1036).
use crate::ports::AudioBackend;
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod trace;
use trace::{AudioTrace, TracedPcm};

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
        pcm.hw_params(&params)
            .map_err(|e| format!("ALSA hw_params: {e}"))?;
        params.get_rate().map_err(|e| format!("ALSA rate: {e}"))
    })?;
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
    trace.stage("write", || {
        for chunk in samples.chunks(CHUNK_FRAMES) {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            match io.writei(chunk) {
                Ok(_) => {}
                Err(e) => {
                    // An underrun is recovered and the chunk retried once.
                    trace
                        .stage("recover", || pcm.try_recover(e, true))
                        .map_err(|e| format!("ALSA play error: {e}"))?;
                    trace
                        .stage("retry_write", || io.writei(chunk))
                        .map_err(|e| format!("ALSA play error: {e}"))?;
                }
            }
        }
        Ok::<(), String>(())
    })?;
    if !cancelled.load(Ordering::SeqCst) {
        let _ = trace.stage("drain", || pcm.drain());
    }
    Ok(())
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
