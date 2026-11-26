// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(feature = "camera-preview", target_os = "linux"))]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use vauchi_linux_hw::HostDevices;
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
    let camera = PipeWireCamera::default();
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

// @internal
#[test]
#[ignore = "requires the desktop PipeWire service and a camera"]
fn the_native_host_publishes_only_its_live_camera_preview() {
    let devices = HostDevices::probe();
    let camera = vauchi_linux_hw::Backends::host(&devices).camera.unwrap();
    let directory = camera.preview_directory().unwrap();
    assert!(directory.resolve("host-test").unwrap().is_none());
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut capture = camera.open_preview("host-test", cancelled.clone()).unwrap();
    let frame = capture.next_luma().unwrap().unwrap();
    assert_eq!(frame.luma.len(), (frame.width * frame.height) as usize);
    let name = PipeWireCamera::preview_node_name("host-test").unwrap();
    assert_eq!(preview_nodes(&name), 1);
    let source = directory.resolve("host-test").unwrap().unwrap();
    assert_ne!(source.node_id, u32::MAX);
    assert_ne!(source.serial, 0);
    assert!(source.remote_fd.is_none());
    assert!(directory.resolve("foreign").unwrap().is_none());
    cancelled.store(true, Ordering::SeqCst);
    assert_eq!(capture.next_luma().unwrap(), None);
    drop(capture);
    assert!(directory.resolve("host-test").unwrap().is_none());
    assert_eq!(preview_nodes(&name), 0);
}

fn preview_nodes(name: &str) -> usize {
    let graph = std::process::Command::new("pw-dump").output().unwrap();
    assert_eq!(graph.status.code(), Some(0));
    let graph: Vec<serde_json::Value> = serde_json::from_slice(&graph.stdout).unwrap();
    graph
        .iter()
        .filter(|node| node["info"]["props"]["node.name"].as_str() == Some(name))
        .count()
}
