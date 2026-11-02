// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contract's audio executor (ADR-077 migration plan, Phase 2).
//!
//! `AudioEmitChallenge` plays Core's samples, `AudioListenForResponse`
//! records for the given time and reports `AudioSamplesRecorded`,
//! `AudioStop` ends both. Emitting and listening are two executors, as a
//! mutual exchange does both at once; each failure is a `HardwareError`
//! on "Audio". The backend work blocks, so it runs on a runtime blocking
//! thread.
use crate::delivery::EventSender;
use crate::executor::Executor;
use crate::ports::{AudioBackend, ExecutorError, HardwareFailure};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::runtime::Handle;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, InboxLimits};

/// Ten seconds of 48 kHz mono: no challenge Core encodes comes near it.
const MAX_SAMPLES: usize = 480_000;
const MAX_LISTEN_MS: u64 = 60_000;

#[derive(Clone)]
struct Emission {
    samples: Arc<Vec<f32>>,
    sample_rate: u32,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Recording {
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
}

pub struct AudioSession {
    emitter: Executor<Emission>,
    listener: Executor<Recording>,
    cancellations: Vec<Arc<AtomicBool>>,
}

impl AudioSession {
    pub fn with_backend(
        backend: Arc<dyn AudioBackend>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let emit_backend = backend.clone();
        let emitter = move |request: Emission, events: EventSender| {
            let backend = emit_backend.clone();
            Box::pin(async move {
                let outcome = tokio::task::spawn_blocking(move || {
                    backend.emit(&request.samples, request.sample_rate, &request.cancelled)
                })
                .await
                .map_err(|_| HardwareFailure::Failed)?;
                match outcome {
                    Ok(()) => Ok(()),
                    Err(error) => report_error(events, error).await,
                }
            }) as _
        };
        let listener = move |request: Recording, events: EventSender| {
            let backend = backend.clone();
            Box::pin(async move {
                let cancelled = request.cancelled.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    backend.record(request.timeout, &request.cancelled)
                })
                .await
                .map_err(|_| HardwareFailure::Failed)?;
                if cancelled.load(Ordering::SeqCst) {
                    return Ok(());
                }
                match outcome {
                    Ok((samples, sample_rate)) => {
                        send(
                            events,
                            Event::AudioSamplesRecorded {
                                samples,
                                sample_rate,
                            },
                        )
                        .await
                    }
                    Err(error) => report_error(events, error).await,
                }
            }) as _
        };
        Ok(Self {
            emitter: Executor::new(emitter, &runtime, limits)?,
            listener: Executor::new(listener, &runtime, limits)?,
            cancellations: Vec::new(),
        })
    }

    /// `AudioEmitChallenge`: play Core's pre-encoded samples.
    pub fn emit_challenge(
        &mut self,
        samples: Vec<f32>,
        sample_rate: u32,
    ) -> Result<(), DeliveryError> {
        self.emitter.ensure_open()?;
        if samples.len() > MAX_SAMPLES || sample_rate == 0 {
            return Err(DeliveryError::PayloadTooLarge);
        }
        let cancelled = self.fresh_cancellation();
        self.emitter.start(Emission {
            samples: Arc::new(samples),
            sample_rate,
            cancelled,
        })
    }

    /// `AudioListenForResponse`: record for `timeout_ms`, then report.
    pub fn listen_for_response(&mut self, timeout_ms: u64) -> Result<(), DeliveryError> {
        self.listener.ensure_open()?;
        if timeout_ms > MAX_LISTEN_MS {
            return Err(DeliveryError::PayloadTooLarge);
        }
        let cancelled = self.fresh_cancellation();
        self.listener.start(Recording {
            timeout: Duration::from_millis(timeout_ms),
            cancelled,
        })
    }

    /// `AudioStop`: end both, reporting nothing.
    pub fn stop(&mut self) {
        self.cancel_all();
        self.emitter.stop();
        self.listener.stop();
    }

    /// The next event from either executor, the listener's first.
    pub fn pop(&mut self) -> Option<Event> {
        self.listener.pop().or_else(|| self.emitter.pop())
    }

    /// Wait for the next event from either executor.
    pub async fn next_event(&mut self) -> Option<Event> {
        tokio::select! {
            Some(event) = self.listener.next_event() => Some(event),
            Some(event) = self.emitter.next_event() => Some(event),
            else => None,
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.cancel_all();
        self.emitter.shutdown().await?;
        self.listener.shutdown().await
    }

    fn fresh_cancellation(&mut self) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancellations.push(cancelled.clone());
        cancelled
    }

    fn cancel_all(&mut self) {
        for cancelled in self.cancellations.drain(..) {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

async fn send(mut events: EventSender, event: Event) -> Result<(), HardwareFailure> {
    events
        .send(event)
        .await
        .map_err(|_| HardwareFailure::Failed)
}

async fn report_error(events: EventSender, error: String) -> Result<(), HardwareFailure> {
    send(
        events,
        Event::HardwareError {
            transport: "Audio".into(),
            error,
        },
    )
    .await
}
