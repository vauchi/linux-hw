// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(feature = "qr-decode")]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use vauchi_contract::{CameraPurpose, Event, executor::InboxLimits};
use vauchi_linux_hw::{CameraBackend, CameraFrames, CameraSession};

struct PreviewCamera(mpsc::UnboundedSender<String>);

impl CameraBackend for PreviewCamera {
    fn open(&self, _: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        Err("capture opened without its preview identity".into())
    }

    fn open_preview(
        &self,
        preview_id: &str,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn CameraFrames>, String> {
        self.0.send(preview_id.into()).unwrap();
        Err("camera permission denied".into())
    }
}

// @scenario: contact_exchange.feature :: Glance scans with the rear camera and can switch to the front camera
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn capture_opens_the_exact_preview_and_reports_permission_denial() {
    let (opened, mut received) = mpsc::unbounded_channel();
    let mut session = CameraSession::with_backend(
        Arc::new(PreviewCamera(opened)),
        tokio::runtime::Handle::current(),
        InboxLimits {
            operations: NonZeroUsize::new(1).unwrap(),
            events: NonZeroUsize::new(16).unwrap(),
            event_bytes: NonZeroUsize::new(65536).unwrap(),
        },
    )
    .unwrap();
    session
        .capture("camera-42".into(), CameraPurpose::QrScan)
        .unwrap();
    assert_eq!(received.recv().await, Some("camera-42".into()));
    assert_eq!(
        session.next_event().await,
        Some(Event::HardwareError {
            transport: "Camera".into(),
            error: "camera permission denied".into(),
        })
    );
    session.shutdown().await.unwrap();
}
