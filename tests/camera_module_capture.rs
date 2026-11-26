// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(target_os = "linux", feature = "fake-camera"))]

use std::{num::NonZeroUsize, os::unix::fs::symlink, time::Duration};
use vauchi_contract::{CameraPurpose, Event, executor::InboxLimits};
use vauchi_linux_hw::{
    CameraSession,
    camera_modules::{BackendChoice, CameraModules},
    frame_directory::FrameResource,
};

fn fixture(mode: &str) -> (tempfile::TempDir, CameraModules) {
    let directory = tempfile::tempdir().unwrap();
    symlink(
        env!("CARGO_BIN_EXE_camera-fixture"),
        directory.path().join("vauchi-camera-v4l2"),
    )
    .unwrap();
    std::fs::write(directory.path().join("fixture-mode"), mode).unwrap();
    write_qr_fixture(directory.path());
    let module =
        CameraModules::discover_in(directory.path(), BackendChoice::Direct, false).unwrap();
    (directory, module)
}

fn write_qr_fixture(folder: &std::path::Path) {
    let code = qrcode::QrCode::new(b"vauchi-test-camera-provider").unwrap();
    let scale = 6;
    let side = (code.width() + 8) * scale;
    let mut rgb = vec![255; side * side * 3];
    for (index, color) in code.to_colors().iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        for dy in 0..scale {
            for dx in 0..scale {
                let x = (index % code.width() + 4) * scale + dx;
                let y = (index / code.width() + 4) * scale + dy;
                rgb[(y * side + x) * 3..(y * side + x) * 3 + 3].fill(0);
            }
        }
    }
    let mut input = (side as u32).to_le_bytes().to_vec();
    input.extend(rgb);
    std::fs::write(folder.join("fixture-rgb"), input).unwrap();
}

fn session(module: CameraModules, runtime: &tokio::runtime::Runtime) -> CameraSession {
    let limits = InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(4).unwrap(),
        event_bytes: NonZeroUsize::new(65536).unwrap(),
    };
    CameraSession::with_module(module, runtime.handle().clone(), limits).unwrap()
}

// @internal
#[test]
fn stopping_and_replacing_a_live_helper_retires_its_readers() {
    use vauchi_linux_hw::{frame_buffer::FrameReader, frame_directory::FrameDirectory};
    async fn resource(directory: &FrameDirectory, previous: u64) -> (u64, FrameReader) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let FrameResource::New { generation, fd } =
                    directory.poll("scan", previous).unwrap()
                {
                    let mut reader = FrameReader::new(std::fs::File::from(fd)).unwrap();
                    loop {
                        if reader.poll().unwrap().is_some() {
                            return (generation, reader);
                        }
                        tokio::task::yield_now().await;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    let (_fixture, module) = fixture("live");
    let directory = module.directory();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut camera = session(module, &runtime);
    camera
        .capture("scan".into(), CameraPurpose::QrScan)
        .unwrap();
    let (generation, mut old) = runtime.block_on(resource(&directory, 0));
    camera
        .capture("scan".into(), CameraPurpose::QrScan)
        .unwrap();
    let (next, mut current) = runtime.block_on(resource(&directory, generation));
    assert!(next > generation);
    old.poll().unwrap();
    assert!(old.closed());
    camera.stop("scan");
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !current.closed() {
                current.poll().unwrap();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    assert!(current.closed());
    assert_eq!(camera.pop(), None);
    runtime.block_on(camera.shutdown()).unwrap();
}

// @internal
#[test]
fn permission_denial_missing_preview_and_foreign_events_report_camera_errors() {
    for mode in ["denied", "blind", "invalid"] {
        let (_fixture, module) = fixture(mode);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let mut camera = session(module, &runtime);
        camera
            .capture("scan".into(), CameraPurpose::QrScan)
            .unwrap();
        let event = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), camera.next_event())
                .await
                .unwrap()
                .unwrap()
        });
        assert!(
            matches!(event, Event::HardwareError { transport, error } if transport == "Camera" && !error.is_empty()),
            "{mode}"
        );
        assert_eq!(camera.pop(), None);
        runtime.block_on(camera.shutdown()).unwrap();
    }
}

// @internal — exercise the actual subprocess, decoder and FD handoff.
#[test]
fn installed_helper_decodes_and_retires_its_native_preview() {
    let (_fixture, module) = fixture("");
    assert!(module.available());
    let resources = module.directory();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let limits = InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(4).unwrap(),
        event_bytes: NonZeroUsize::new(65536).unwrap(),
    };
    let mut camera = CameraSession::with_module(module, runtime.handle().clone(), limits).unwrap();
    camera
        .capture("scan".into(), CameraPurpose::QrScan)
        .unwrap();
    let event = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), camera.next_event())
            .await
            .unwrap()
            .unwrap()
    });
    assert_eq!(
        event,
        Event::QrScanned {
            data: "vauchi-test-camera-provider".into()
        }
    );
    runtime.block_on(camera.shutdown()).unwrap();
    assert!(matches!(
        resources.poll("scan", 0).unwrap(),
        FrameResource::Absent
    ));
}
