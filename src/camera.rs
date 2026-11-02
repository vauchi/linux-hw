// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contract's camera executor (ADR-077 migration plan, Phase 2).
//!
//! `CameraCapture` with purpose `QrScan` reads luma frames from the
//! camera port on a runtime blocking thread and decodes each with
//! `qr::scan_qr_from_luma` until one carries a code, reported once as
//! `QrScanned`; `CameraStop` cancels the capture and reports nothing. A
//! camera that cannot open, or a purpose this shell has no port for, is a
//! `HardwareError` on "Camera". The preview is not here: the ADR-077
//! amendment makes it a PipeWire stream, its own record.
use crate::delivery::EventSender;
use crate::executor::Executor;
use crate::ports::{CameraBackend, ExecutorError, HardwareFailure};
use crate::qr::{ScannerBackend, scan_qr_from_luma};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::runtime::Handle;
use vauchi_contract::CameraPurpose;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, InboxLimits};

const MAX_PREVIEW_ID: usize = 128;

#[derive(Clone)]
struct Capture {
    purpose: CameraPurpose,
    cancelled: Arc<AtomicBool>,
}

pub struct CameraSession {
    executor: Executor<Capture>,
    active: Option<(String, Arc<AtomicBool>)>,
}

impl CameraSession {
    pub fn with_backend(
        backend: Arc<dyn CameraBackend>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let native = move |request: Capture, events: EventSender| {
            let backend = backend.clone();
            Box::pin(async move {
                match request.purpose {
                    CameraPurpose::QrScan => scan(backend, request.cancelled, events).await,
                    other => {
                        report_error(
                            events,
                            format!("camera purpose {other:?} has no port on Linux"),
                        )
                        .await
                    }
                }
            }) as _
        };
        Ok(Self {
            executor: Executor::new(native, &runtime, limits)?,
            active: None,
        })
    }

    /// `CameraCapture { preview_id, purpose }`.
    pub fn capture(
        &mut self,
        preview_id: String,
        purpose: CameraPurpose,
    ) -> Result<(), DeliveryError> {
        self.executor.ensure_open()?;
        if preview_id.len() > MAX_PREVIEW_ID {
            return Err(DeliveryError::PayloadTooLarge);
        }
        self.cancel_active();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.executor.start(Capture {
            purpose,
            cancelled: cancelled.clone(),
        })?;
        self.active = Some((preview_id, cancelled));
        Ok(())
    }

    /// `CameraStop { preview_id }`: ends the capture of that preview and
    /// reports nothing; another preview's id is not this capture.
    pub fn stop(&mut self, preview_id: &str) {
        if self.active.as_ref().is_some_and(|(id, _)| id == preview_id) {
            self.cancel_active();
            self.executor.stop();
        }
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.executor.pop()
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.executor.next_event().await
    }

    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.cancel_active();
        self.executor.shutdown().await
    }

    fn cancel_active(&mut self) {
        if let Some((_, cancelled)) = self.active.take() {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for CameraSession {
    fn drop(&mut self) {
        self.cancel_active();
    }
}

async fn scan(
    backend: Arc<dyn CameraBackend>,
    cancelled: Arc<AtomicBool>,
    events: EventSender,
) -> Result<(), HardwareFailure> {
    let stop = cancelled.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
        let mut frames = backend.open(stop.clone())?;
        while !stop.load(Ordering::SeqCst) {
            let Some(frame) = frames.next_luma()? else {
                return Ok(None);
            };
            let scan = scan_qr_from_luma(
                ScannerBackend::RqrrPreprocessed,
                &frame.luma,
                frame.width,
                frame.height,
            );
            if let Some(code) = scan.decoded {
                return Ok(Some(code));
            }
        }
        Ok(None)
    })
    .await
    .map_err(|_| HardwareFailure::Failed)?;
    if cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    match outcome {
        Ok(Some(data)) => send(events, Event::QrScanned { data }).await,
        Ok(None) => Ok(()),
        Err(error) => report_error(events, error).await,
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
            transport: "Camera".into(),
            error,
        },
    )
    .await
}

/// Pack an RGB frame into the 8-bit Y plane the decoder expects: BT.601
/// luma weights, integer arithmetic to stay allocation-free per pixel.
pub fn rgb_to_luma(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|p| {
            let (r, g, b) = (u32::from(p[0]), u32::from(p[1]), u32::from(p[2]));
            ((r * 299 + g * 587 + b * 114) / 1000) as u8
        })
        .collect()
}
