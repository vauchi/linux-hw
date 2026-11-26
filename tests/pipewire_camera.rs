// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(feature = "camera-preview", target_os = "linux"))]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use vauchi_linux_hw::{CameraBackend, PipeWireCamera};

// @internal
#[test]
fn preview_names_are_scoped_to_the_app_and_encode_identity_without_nul_bytes() {
    let name = PipeWireCamera::preview_node_name("camera-42").unwrap();
    assert_eq!(
        name,
        format!("vauchi.camera.{}.63616d6572612d3432", std::process::id())
    );
    assert_ne!(
        name,
        PipeWireCamera::preview_node_name("camera-43").unwrap()
    );
    assert_eq!(
        PipeWireCamera::preview_node_name(""),
        Err("invalid camera preview identity")
    );
    assert_eq!(
        PipeWireCamera::preview_node_name(&"x".repeat(129)),
        Err("invalid camera preview identity")
    );
}

// @scenario: contact_exchange.feature :: Glance scans with the rear camera and can switch to the front camera
#[test]
#[ignore = "requires the desktop PipeWire service and a camera"]
fn native_capture_provides_a_frame_then_honors_stop() {
    let camera = PipeWireCamera;
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut capture = camera
        .open_preview("native-camera-test", cancelled.clone())
        .unwrap();
    let frame = capture.next_luma().unwrap().unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.luma.len()),
        (640, 480, 640 * 480)
    );
    cancelled.store(true, Ordering::SeqCst);
    assert_eq!(capture.next_luma().unwrap(), None);
}
