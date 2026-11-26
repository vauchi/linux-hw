// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(target_os = "linux", feature = "camera-modules"))]

use std::{fs::File, sync::Arc};
use vauchi_linux_hw::{
    frame_buffer::{FrameReader, FrameWriter},
    frame_directory::{FrameDirectory, FrameResource},
};

fn writer() -> Arc<FrameWriter> {
    Arc::new(FrameWriter::initialize(tempfile::tempfile().unwrap()).unwrap())
}

// @internal — opaque resource handoff, never camera bytes through Core.
#[test]
fn replacing_the_same_preview_id_changes_generation_and_retires_old_readers() {
    let directory = FrameDirectory::default();
    let old = directory.publish("scan", writer()).unwrap();
    let FrameResource::New { generation, fd } = directory.poll("scan", 0).unwrap() else {
        panic!("new capture must supply a resource");
    };
    let mut reader = FrameReader::new(File::from(fd)).unwrap();
    assert!(matches!(
        directory.poll("scan", generation).unwrap(),
        FrameResource::Unchanged
    ));
    let new = directory.publish("scan", writer()).unwrap();
    reader.poll().unwrap();
    assert!(reader.closed());
    drop(old);
    let FrameResource::New {
        generation: next, ..
    } = directory.poll("scan", generation).unwrap()
    else {
        panic!("replacement must supply a new resource");
    };
    assert!(next > generation);
    drop(new);
    assert!(matches!(
        directory.poll("scan", next).unwrap(),
        FrameResource::Absent
    ));
}

// @internal
#[test]
fn resources_are_isolated_and_ids_validated() {
    let first = FrameDirectory::default();
    let second = FrameDirectory::default();
    let file = writer();
    let _lease = first.publish("scan", file.clone()).unwrap();
    assert!(first.publish("scan", file).is_err());
    for id in ["", &"x".repeat(129)] {
        assert!(first.publish(id, writer()).is_err());
    }
    assert!(matches!(
        first.poll("other", 0).unwrap(),
        FrameResource::Absent
    ));
    assert!(matches!(
        second.poll("scan", 0).unwrap(),
        FrameResource::Absent
    ));
}
