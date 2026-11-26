// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(feature = "camera-preview", target_os = "linux"))]

mod portal_fixture;

use portal_fixture::{FakePortal, Response};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use vauchi_linux_hw::camera_portal::{CameraPortal, PortalError};
use vauchi_linux_hw::{CameraBackend, PipeWireCamera};

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_immediate_grant_is_received_and_opens_separate_remote_connections() {
    let fixture = FakePortal::start(Response::Grant).await;
    let portal = CameraPortal::new(fixture.client.clone());
    assert!(portal.is_camera_present().await.unwrap());
    let grant = portal
        .request(Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    let mut first = UnixStream::from(grant.open_remote().await.unwrap());
    let mut second = UnixStream::from(grant.open_remote().await.unwrap());
    let mut peers = fixture.peers.lock().unwrap();
    assert_eq!(peers.len(), 2);
    first.write_all(b"a").unwrap();
    second.write_all(b"b").unwrap();
    let mut value = [0];
    peers[0].read_exact(&mut value).unwrap();
    assert_eq!(value, *b"a");
    peers[1].read_exact(&mut value).unwrap();
    assert_eq!(value, *b"b");
    assert_eq!(fixture.accesses.load(Ordering::SeqCst), 1);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denial_does_not_open_a_remote() {
    let fixture = FakePortal::start(Response::Deny).await;
    let portal = CameraPortal::new(fixture.client.clone());
    assert_eq!(
        portal.request(Arc::new(AtomicBool::new(false))).await.err(),
        Some(PortalError::Denied)
    );
    assert_eq!(fixture.peers.lock().unwrap().len(), 0);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_permission_response_never_grants_access() {
    let fixture = FakePortal::start(Response::Malformed).await;
    let portal = CameraPortal::new(fixture.client.clone());
    assert_eq!(
        portal.request(Arc::new(AtomicBool::new(false))).await.err(),
        Some(PortalError::InvalidResponse)
    );
    assert_eq!(fixture.peers.lock().unwrap().len(), 0);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_closes_the_pending_request_without_opening_a_remote() {
    let fixture = FakePortal::start(Response::Wait).await;
    let portal = CameraPortal::new(fixture.client.clone());
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let request = tokio::spawn(async move { portal.request(stop).await });
    fixture.opened.notified().await;
    cancelled.store(true, Ordering::SeqCst);
    assert_eq!(request.await.unwrap().err(), Some(PortalError::Cancelled));
    assert_eq!(fixture.closes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peers.lock().unwrap().len(), 0);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_also_closes_a_request_whose_method_reply_is_stalled() {
    let fixture = FakePortal::start(Response::HoldReply).await;
    let portal = CameraPortal::new(fixture.client.clone());
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let request = tokio::spawn(async move { portal.request(stop).await });
    fixture.opened.notified().await;
    cancelled.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), request).await;
    assert_eq!(result.unwrap().unwrap().err(), Some(PortalError::Cancelled));
    assert_eq!(fixture.closes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peers.lock().unwrap().len(), 0);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_required_portal_never_falls_back_to_native_capture_after_denial() {
    let fixture = FakePortal::start(Response::Deny).await;
    let camera = PipeWireCamera::with_portal(CameraPortal::new(fixture.client.clone()), true);
    let error = tokio::task::spawn_blocking(move || {
        camera
            .open_preview("denied", Arc::new(AtomicBool::new(false)))
            .err()
    })
    .await
    .unwrap();
    assert_eq!(error.as_deref(), Some("camera access denied"));
    assert_eq!(fixture.peers.lock().unwrap().len(), 0);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sandbox_without_a_camera_portal_does_not_try_the_default_remote() {
    let fixture = FakePortal::start(Response::Unavailable).await;
    let camera = PipeWireCamera::with_portal(CameraPortal::new(fixture.client.clone()), true);
    let error = tokio::task::spawn_blocking(move || {
        camera
            .open_preview("missing", Arc::new(AtomicBool::new(false)))
            .err()
    })
    .await
    .unwrap();
    assert_eq!(error.as_deref(), Some("camera portal unavailable"));
    assert_eq!(fixture.accesses.load(Ordering::SeqCst), 0);
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probing_an_absent_camera_does_not_request_camera_access() {
    let fixture = FakePortal::start(Response::NoCamera).await;
    let camera = PipeWireCamera::with_portal(CameraPortal::new(fixture.client.clone()), true);
    let error = tokio::task::spawn_blocking(move || {
        camera
            .open_preview("absent", Arc::new(AtomicBool::new(false)))
            .err()
    })
    .await
    .unwrap();
    assert_eq!(error.as_deref(), Some("no camera is available"));
    assert_eq!(fixture.accesses.load(Ordering::SeqCst), 0);
}

// @internal — real PipeWire capture over a DBus-transferred FD, with the
// default remote deliberately unavailable in the invoking environment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires VAUCHI_TEST_PIPEWIRE_SOCKET and a real desktop camera"]
async fn portal_capture_uses_the_transferred_fd_and_publishes_fresh_preview_remotes() {
    let fixture = FakePortal::start(Response::NativeGrant).await;
    let camera = PipeWireCamera::with_portal(CameraPortal::new(fixture.client.clone()), true);
    let directory = camera.preview_directory();
    let worker = tokio::task::spawn_blocking(move || {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut capture = camera
            .open_preview("portal-proof", cancelled.clone())
            .unwrap();
        let frame = capture.next_luma().unwrap().unwrap();
        assert_eq!(
            (frame.width, frame.height, frame.luma.len()),
            (640, 480, 640 * 480)
        );
        let first = directory.resolve("portal-proof").unwrap().unwrap();
        let second = directory.resolve("portal-proof").unwrap().unwrap();
        assert_eq!(
            (first.node_id, first.serial),
            (second.node_id, second.serial)
        );
        assert_ne!(first.node_id, u32::MAX);
        assert!(first.remote_fd.is_some());
        assert!(second.remote_fd.is_some());
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(capture.next_luma().unwrap(), None);
        drop(capture);
        assert!(directory.resolve("portal-proof").unwrap().is_none());
    });
    worker.await.unwrap();
    assert_eq!(fixture.accesses.load(Ordering::SeqCst), 1);
}
