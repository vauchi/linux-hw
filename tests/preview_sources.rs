// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(feature = "camera-preview", target_os = "linux"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{Mutex, mpsc},
};
use vauchi_linux_hw::preview_source::{PreviewDirectory, SourceError};

// @internal
#[test]
fn equal_core_ids_in_different_apps_resolve_only_their_own_stream() {
    let first = PreviewDirectory::default();
    let second = PreviewDirectory::default();
    let first_lease = first.publish("camera-1", 10, 100, None).unwrap();
    let _second_lease = second.publish("camera-1", 20, 200, None).unwrap();
    let source = first.resolve("camera-1").unwrap().unwrap();
    assert_eq!((source.node_id, source.serial), (10, 100));
    let source = second.resolve("camera-1").unwrap().unwrap();
    assert_eq!((source.node_id, source.serial), (20, 200));
    drop(first_lease);
    assert_eq!(first.resolve("camera-1").unwrap().map(|s| s.serial), None);
    assert_eq!(second.resolve("camera-1").unwrap().unwrap().serial, 200);
}

// @internal
#[test]
fn dropping_an_old_capture_cannot_retire_its_replacement() {
    let sources = PreviewDirectory::default();
    let old = sources.publish("camera-1", 10, 100, None).unwrap();
    let current = sources.publish("camera-2", 20, 200, None).unwrap();
    drop(old);
    assert_eq!(sources.resolve("camera-1").unwrap().map(|s| s.serial), None);
    assert_eq!(sources.resolve("camera-2").unwrap().unwrap().serial, 200);
    drop(current);
    assert_eq!(sources.resolve("camera-2").unwrap().map(|s| s.serial), None);
}

// @internal
#[test]
fn each_portal_consumer_gets_a_new_remote_and_denial_never_falls_back() {
    let sources = PreviewDirectory::default();
    let opens = Arc::new(AtomicUsize::new(0));
    let opened = opens.clone();
    let _lease = sources
        .publish(
            "camera-1",
            10,
            100,
            Some(Arc::new(move || {
                opened.fetch_add(1, Ordering::SeqCst);
                Err("camera access denied".into())
            })),
        )
        .unwrap();
    assert_eq!(
        sources.resolve("camera-1").err(),
        Some(SourceError::Remote("camera access denied".into()))
    );
    assert_eq!(
        sources.resolve("camera-1").err(),
        Some(SourceError::Remote("camera access denied".into()))
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
    assert_eq!(
        sources.resolve("old-camera").unwrap().map(|s| s.serial),
        None
    );
    assert_eq!(opens.load(Ordering::SeqCst), 2);
}

// @internal
#[test]
fn invalid_preview_sources_are_rejected() {
    let sources = PreviewDirectory::default();
    for (id, node, serial) in [
        ("", 10, 100),
        ("camera-1", u32::MAX, 100),
        ("camera-1", 10, 0),
    ] {
        assert_eq!(
            sources.publish(id, node, serial, None).err(),
            Some(SourceError::Invalid)
        );
    }
    assert_eq!(sources.resolve("camera-1").unwrap().map(|s| s.serial), None);
}

// @internal
#[test]
fn each_resolved_remote_is_an_independent_protocol_connection() {
    let sources = PreviewDirectory::default();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let opened = peers.clone();
    let _lease = sources
        .publish(
            "camera-1",
            10,
            100,
            Some(Arc::new(move || {
                let (socket, peer) = UnixStream::pair().unwrap();
                opened.lock().unwrap().push(peer);
                Ok(socket.into())
            })),
        )
        .unwrap();
    let mut first = UnixStream::from(
        sources
            .resolve("camera-1")
            .unwrap()
            .unwrap()
            .remote_fd
            .unwrap(),
    );
    let mut second = UnixStream::from(
        sources
            .resolve("camera-1")
            .unwrap()
            .unwrap()
            .remote_fd
            .unwrap(),
    );
    first.write_all(b"a").unwrap();
    second.write_all(b"b").unwrap();
    let mut peers = peers.lock().unwrap();
    assert_eq!(peers.len(), 2);
    let mut value = [0];
    peers[0].read_exact(&mut value).unwrap();
    assert_eq!(value, *b"a");
    peers[1].read_exact(&mut value).unwrap();
    assert_eq!(value, *b"b");
}

// @internal
#[test]
fn a_remote_open_finishing_after_capture_retirement_is_discarded() {
    let sources = PreviewDirectory::default();
    let (started, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let lease = sources
        .publish(
            "camera-1",
            10,
            100,
            Some(Arc::new(move || {
                started.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                Ok(UnixStream::pair().unwrap().0.into())
            })),
        )
        .unwrap();
    let reading = sources.clone();
    let worker = std::thread::spawn(move || reading.resolve("camera-1"));
    ready.recv().unwrap();
    drop(lease);
    release.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap().map(|source| source.serial),
        None
    );
}
