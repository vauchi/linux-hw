// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The cpal devices behind the audio port, moved from Core's
//! `exchange/audio_cpal.rs` (owner, 2026-11-02): the default output plays
//! the samples, the default input records at its own rate.
use crate::ports::AudioBackend;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(10);

pub struct CpalAudio;

impl AudioBackend for CpalAudio {
    fn emit(
        &self,
        samples: &[f32],
        sample_rate: u32,
        cancelled: &AtomicBool,
    ) -> Result<(), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No output device".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("Config error: {e}"))?;
        let queue = Arc::new(Mutex::new((samples.to_vec(), 0usize)));
        let done = Arc::new(AtomicBool::new(false));
        let stream = {
            let queue = queue.clone();
            let done = done.clone();
            device
                .build_output_stream(
                    &config.into(),
                    move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let mut queue = queue.lock().unwrap_or_else(|p| p.into_inner());
                        let (samples, index) = &mut *queue;
                        for sample in output.iter_mut() {
                            if *index < samples.len() {
                                *sample = samples[*index];
                                *index += 1;
                            } else {
                                *sample = 0.0;
                                done.store(true, Ordering::SeqCst);
                            }
                        }
                    },
                    |_err| {},
                    None,
                )
                .map_err(|e| format!("Stream error: {e}"))?
        };
        stream.play().map_err(|e| format!("Play error: {e}"))?;
        // The device may never drain a stream it cannot play; the sample
        // count bounds the wait.
        let budget = Duration::from_millis(
            samples.len() as u64 * 1000 / u64::from(sample_rate.max(1)) + 100,
        );
        let start = Instant::now();
        while !done.load(Ordering::SeqCst)
            && !cancelled.load(Ordering::SeqCst)
            && start.elapsed() < budget
        {
            std::thread::sleep(POLL);
        }
        Ok(())
    }

    fn record(&self, timeout: Duration, cancelled: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "No input device".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("Config error: {e}"))?;
        let sample_rate = config.sample_rate().0;
        let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
        let stream = {
            let recorded = recorded.clone();
            device
                .build_input_stream(
                    &config.into(),
                    move |input: &[f32], _: &cpal::InputCallbackInfo| {
                        recorded
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .extend_from_slice(input);
                    },
                    |_err| {},
                    None,
                )
                .map_err(|e| format!("Stream error: {e}"))?
        };
        stream.play().map_err(|e| format!("Record error: {e}"))?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && !cancelled.load(Ordering::SeqCst) {
            std::thread::sleep(POLL);
        }
        drop(stream);
        let samples = recorded.lock().unwrap_or_else(|p| p.into_inner()).clone();
        Ok((samples, sample_rate))
    }
}
