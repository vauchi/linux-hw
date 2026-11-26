// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(target_os = "linux", feature = "qr-decode"))]

use std::{fs::File, io::ErrorKind, os::unix::fs::FileExt};
use vauchi_linux_hw::{
    camera_frame::{CameraFrame, PixelFormat},
    frame_buffer::{FrameReader, FrameWriter},
};

fn frame(value: u8) -> CameraFrame {
    CameraFrame::from_raw(PixelFormat::Rgba, 2, 1, 8, 0, &[value; 8]).unwrap()
}

// @internal — backend-neutral native preview resources.
#[test]
fn frame_buffer_keeps_only_the_latest_frame_and_closes_its_readers() {
    let writer = FrameWriter::initialize(tempfile::tempfile().unwrap()).unwrap();
    let mut reader = writer.reader().unwrap();
    assert_eq!(reader.poll().unwrap(), None);
    assert!(writer.write(&frame(12)).unwrap());
    assert!(writer.write(&frame(23)).unwrap());
    assert_eq!(reader.poll().unwrap(), Some(frame(23)));
    assert_eq!(reader.poll().unwrap(), None);
    writer.close().unwrap();
    assert_eq!(reader.poll().unwrap(), None);
    assert!(reader.closed());
}

// @internal
#[test]
fn incomplete_frame_files_are_rejected_before_polling() {
    let error = FrameReader::new(tempfile::tempfile().unwrap())
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

// @internal
#[test]
fn malformed_frame_headers_fail_before_allocating_pixels() {
    for (offset, bytes) in [
        (0, vec![0; 8]),
        (8, u32::MAX.to_le_bytes().to_vec()),
        (12, 0u32.to_le_bytes().to_vec()),
        (16, u32::MAX.to_le_bytes().to_vec()),
        (20, 99u32.to_le_bytes().to_vec()),
        (24, 0u64.to_le_bytes().to_vec()),
    ] {
        let file = tempfile::tempfile().unwrap();
        let corrupt = file.try_clone().unwrap();
        let writer = FrameWriter::initialize(file).unwrap();
        assert!(writer.write(&frame(12)).unwrap());
        let mut reader = writer.reader().unwrap();
        corrupt.write_all_at(&bytes, offset).unwrap();
        assert_eq!(reader.poll().unwrap_err().kind(), ErrorKind::InvalidData);
    }
}

// @internal
#[test]
fn preview_contention_never_blocks_a_reader_or_writer() {
    let writer = FrameWriter::initialize(tempfile::tempfile().unwrap()).unwrap();
    let lock = File::from(writer.reader_fd().unwrap());
    let mut reader = writer.reader().unwrap();
    lock.lock().unwrap();
    assert!(!writer.write(&frame(12)).unwrap());
    assert_eq!(reader.poll().unwrap(), None);
    lock.unlock().unwrap();
    assert!(writer.write(&frame(23)).unwrap());
    assert_eq!(reader.poll().unwrap(), Some(frame(23)));
}

// @internal
#[test]
fn dropping_the_publisher_retires_a_still_open_preview_descriptor() {
    let mut reader = {
        let writer = FrameWriter::initialize(tempfile::tempfile().unwrap()).unwrap();
        let mut reader = writer.reader().unwrap();
        assert!(writer.write(&frame(23)).unwrap());
        assert_eq!(reader.poll().unwrap(), Some(frame(23)));
        reader
    };
    assert_eq!(reader.poll().unwrap(), None);
    assert!(reader.closed());
}
