// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `CameraSession` is the contract's camera executor: `CameraCapture` with
//! purpose `QrScan` reads luma frames from the camera port until one
//! decodes and reports `QrScanned` once; `CameraStop` ends the capture and
//! reports nothing. A camera that cannot open, or a purpose this shell
//! has no port for, is a `HardwareError` on "Camera". No preview yet
//! (ADR-077 amendment: the preview is a PipeWire stream, its own record).
#![cfg(feature = "qr-decode")]
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vauchi_contract::CameraPurpose;
use vauchi_contract::Event;
use vauchi_contract::executor::InboxLimits;
use vauchi_linux_hw::qr::{ScannerBackend, scan_qr_from_luma};
use vauchi_linux_hw::{CameraBackend, CameraFrames, CameraSession, LumaFrame, rgb_to_luma};

/// Render `payload` as a QR code into an RGB buffer, one `scale`-sized
/// square per module plus a 4-module quiet zone (the spec minimum —
/// without it decoders cannot find the finder patterns).
fn render_qr_rgb(payload: &str, scale: usize) -> (Vec<u8>, u32) {
    const QUIET_MODULES: usize = 4;
    let code = qrcode::QrCode::new(payload).expect("payload fits a QR code");
    let modules = code.to_colors();
    let side_modules = code.width() + 2 * QUIET_MODULES;
    let side_px = side_modules * scale;
    let mut rgb = vec![255u8; side_px * side_px * 3];
    for (i, color) in modules.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let (mx, my) = (i % code.width(), i / code.width());
        for dy in 0..scale {
            for dx in 0..scale {
                let x = (mx + QUIET_MODULES) * scale + dx;
                let y = (my + QUIET_MODULES) * scale + dy;
                let at = (y * side_px + x) * 3;
                rgb[at..at + 3].fill(0);
            }
        }
    }
    (rgb, side_px as u32)
}

fn qr_frame(payload: &str) -> LumaFrame {
    let (rgb, side) = render_qr_rgb(payload, 6);
    LumaFrame {
        luma: rgb_to_luma(&rgb),
        width: side,
        height: side,
    }
}

fn noise_frame() -> LumaFrame {
    let side = 120u32;
    let luma = (0..side * side).map(|i| (i * 7919 % 251) as u8).collect();
    LumaFrame {
        luma,
        width: side,
        height: side,
    }
}

/// A camera whose frames are scripted; after the script it blocks until
/// cancelled, as a live camera would keep streaming.
struct ScriptedCamera {
    frames: Mutex<Vec<LumaFrame>>,
    open_fails: Option<String>,
    opened: AtomicUsize,
    read: Arc<AtomicUsize>,
}

struct ScriptedFrames {
    frames: Vec<LumaFrame>,
    read: Arc<AtomicUsize>,
    cancelled: Arc<AtomicBool>,
}

impl CameraFrames for ScriptedFrames {
    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String> {
        if self.frames.is_empty() {
            while !self.cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            return Ok(None);
        }
        self.read.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.frames.remove(0)))
    }
}

impl CameraBackend for ScriptedCamera {
    fn open(&self, cancelled: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        if let Some(reason) = &self.open_fails {
            return Err(reason.clone());
        }
        Ok(Box::new(ScriptedFrames {
            frames: std::mem::take(&mut *self.frames.lock().unwrap()),
            read: self.read.clone(),
            cancelled,
        }))
    }
}

fn camera(frames: Vec<LumaFrame>) -> Arc<ScriptedCamera> {
    Arc::new(ScriptedCamera {
        frames: Mutex::new(frames),
        open_fails: None,
        opened: AtomicUsize::new(0),
        read: Arc::new(AtomicUsize::new(0)),
    })
}

fn session(backend: Arc<ScriptedCamera>, runtime: &tokio::runtime::Runtime) -> CameraSession {
    let limits = InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(16).unwrap(),
        event_bytes: NonZeroUsize::new(1 << 16).unwrap(),
    };
    CameraSession::with_backend(backend, runtime.handle().clone(), limits).unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

fn drain(session: &mut CameraSession, expected: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    while events.len() < expected && Instant::now() < deadline {
        match session.pop() {
            Some(event) => events.push(event),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    events
}

fn camera_error(error: &str) -> Event {
    Event::HardwareError {
        transport: "Camera".into(),
        error: error.into(),
    }
}

const PAYLOAD: &str = "vauchi://exchange/oNZq7xR2";

// @scenario: contact_exchange.feature :: Successful QR code exchange with proximity
#[test]
fn capture_scans_frames_until_one_decodes_and_reports_the_code_once() {
    let runtime = runtime();
    let backend = camera(vec![
        noise_frame(),
        noise_frame(),
        qr_frame(PAYLOAD),
        qr_frame(PAYLOAD),
    ]);
    let mut session = session(backend.clone(), &runtime);
    session
        .capture("preview-1".into(), CameraPurpose::QrScan)
        .unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![Event::QrScanned {
            data: PAYLOAD.into()
        }]
    );
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(session.pop(), None, "one code, one report");
    assert_eq!(
        backend.read.load(Ordering::SeqCst),
        3,
        "reading stops at the first decode"
    );
}

// @scenario: contact_exchange.feature :: Glance scans with the rear camera and can switch to the front camera
#[test]
fn a_camera_that_cannot_open_reports_why_on_camera() {
    let runtime = runtime();
    let backend = Arc::new(ScriptedCamera {
        frames: Mutex::new(Vec::new()),
        open_fails: Some("Camera open failed: no device".into()),
        opened: AtomicUsize::new(0),
        read: Arc::new(AtomicUsize::new(0)),
    });
    let mut session = session(backend, &runtime);
    session
        .capture("preview-1".into(), CameraPurpose::QrScan)
        .unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![camera_error("Camera open failed: no device")]
    );
}

// @internal
#[test]
fn stop_ends_the_capture_without_a_report() {
    let runtime = runtime();
    let backend = camera(vec![noise_frame()]);
    let mut session = session(backend.clone(), &runtime);
    session
        .capture("preview-1".into(), CameraPurpose::QrScan)
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    session.stop("preview-1");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(session.pop(), None);
    assert_eq!(backend.opened.load(Ordering::SeqCst), 1);
}

// @internal
#[test]
fn a_purpose_this_shell_has_no_port_for_is_reported_not_guessed() {
    let runtime = runtime();
    let backend = camera(Vec::new());
    let mut session = session(backend.clone(), &runtime);
    session
        .capture("preview-1".into(), CameraPurpose::Photo)
        .unwrap();
    assert_eq!(
        drain(&mut session, 1),
        vec![camera_error("camera purpose Photo has no port on Linux")]
    );
    assert_eq!(backend.opened.load(Ordering::SeqCst), 0);
}

// @internal
#[test]
fn the_decoder_reads_a_frame_converted_by_the_luma_packer() {
    let (rgb, side) = render_qr_rgb(PAYLOAD, 6);
    let luma = rgb_to_luma(&rgb);
    let scan = scan_qr_from_luma(ScannerBackend::RqrrPreprocessed, &luma, side, side);
    assert_eq!(scan.decoded.as_deref(), Some(PAYLOAD));
}

// @internal
#[test]
fn an_inverted_frame_does_not_decode() {
    let (rgb, side) = render_qr_rgb(PAYLOAD, 6);
    let inverted: Vec<u8> = rgb.iter().map(|b| 255 - b).collect();
    let luma = rgb_to_luma(&inverted);
    let scan = scan_qr_from_luma(ScannerBackend::RqrrPreprocessed, &luma, side, side);
    assert_eq!(scan.decoded, None);
}

// @internal
#[test]
fn the_luma_packer_emits_one_byte_per_pixel() {
    let rgb = [255, 255, 255, 0, 0, 0, 255, 0, 0];
    assert_eq!(rgb_to_luma(&rgb), vec![255, 0, 76]);
}
