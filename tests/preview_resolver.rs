// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(feature = "camera-preview", target_os = "linux"))]

use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use vauchi_linux_hw::preview_source::{PreviewDirectory, PreviewResolver};

// @internal
#[test]
fn waiting_for_a_portal_remote_never_blocks_the_display_and_stale_results_are_discarded() {
    let directory = PreviewDirectory::default();
    let (started, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let lease = directory
        .publish(
            "first",
            10,
            100,
            Some(Arc::new(move || {
                started.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                Ok(UnixStream::pair().unwrap().0.into())
            })),
        )
        .unwrap();
    let mut resolver = directory.resolver().unwrap();
    assert!(resolver.poll("first").unwrap().is_none());
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(resolver.poll("first").unwrap().is_none());
    drop(lease);
    let _next = directory.publish("second", 20, 200, None).unwrap();
    assert!(resolver.poll("second").unwrap().is_none());
    release.send(()).unwrap();
    let source = await_source(&mut resolver, "second");
    assert_eq!((source.node_id, source.serial), (20, 200));
    assert!(source.remote_fd.is_none());
}

// @internal
#[test]
fn a_failed_remote_is_reported_without_a_native_descriptor() {
    let directory = PreviewDirectory::default();
    let _lease = directory
        .publish("denied", 10, 100, Some(Arc::new(|| Err("denied".into()))))
        .unwrap();
    let mut resolver = directory.resolver().unwrap();
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(1));
        match resolver.poll("denied") {
            Err(error) => {
                assert_eq!(error.to_string(), "denied");
                break;
            }
            Ok(None) => std::thread::yield_now(),
            Ok(Some(_)) => panic!("denial must not yield a native descriptor"),
        }
    }
}

fn await_source(
    resolver: &mut PreviewResolver,
    id: &str,
) -> vauchi_linux_hw::preview_source::PreviewSource {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(1));
        match resolver.poll(id).unwrap() {
            Some(source) => return source,
            None => std::thread::yield_now(),
        }
    }
}
