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
