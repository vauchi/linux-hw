// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(feature = "qr-decode")]

use vauchi_linux_hw::camera_frame::{CameraFrame, FrameError, PixelFormat};

// @internal
#[test]
fn padded_offset_rows_produce_the_same_preview_and_decoder_pixels() {
    let bytes = [99, 255, 0, 0, 0, 255, 0, 88, 88, 0, 0, 255, 255, 255, 255];
    let frame = CameraFrame::from_raw(PixelFormat::Rgb, 2, 2, 8, 1, &bytes).unwrap();
    assert_eq!(
        frame.rgba(),
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255
        ]
    );
    assert_eq!(frame.luma(), vec![76, 149, 29, 255]);
    assert_eq!((frame.width(), frame.height()), (2, 2));
}

// @internal
#[test]
fn yuy2_uses_both_pixels_and_the_shared_chroma() {
    let frame = CameraFrame::from_raw(PixelFormat::Yuy2, 2, 1, 4, 0, &[16, 128, 235, 128]).unwrap();
    assert_eq!(frame.rgba(), &[0, 0, 0, 255, 255, 255, 255, 255]);
    assert_eq!(frame.luma(), vec![0, 255]);
}

// @internal
#[test]
fn invalid_frame_layouts_fail_before_allocating_or_reading_pixels() {
    for (width, height, stride, offset, expected) in [
        (0, 1, 3, 0, FrameError::Dimensions),
        (1, 0, 3, 0, FrameError::Dimensions),
        (u32::MAX, 2, 3, 0, FrameError::Dimensions),
        (1920, 1081, 5760, 0, FrameError::Dimensions),
        (2, 1, 3, 0, FrameError::Stride),
        (1, 1, 3, usize::MAX, FrameError::Buffer),
        (1, 2, usize::MAX, 0, FrameError::Buffer),
        (1, 1, 3, 4, FrameError::Buffer),
    ] {
        assert_eq!(
            CameraFrame::from_raw(PixelFormat::Rgb, width, height, stride, offset, &[0; 6]),
            Err(expected)
        );
    }
    assert_eq!(
        CameraFrame::from_raw(PixelFormat::Yuy2, 1, 1, 4, 0, &[0; 4]),
        Err(FrameError::Dimensions)
    );
}

// @internal
#[test]
fn row_padding_and_prefix_bytes_never_change_visible_pixels() {
    for padding in 0..8 {
        for offset in 0..8 {
            let stride = 4 + padding;
            let mut bytes = vec![77; offset + stride + 4];
            bytes[offset..offset + 4].copy_from_slice(&[10, 20, 30, 0]);
            bytes[offset + stride..offset + stride + 4].copy_from_slice(&[40, 50, 60, 0]);
            let frame =
                CameraFrame::from_raw(PixelFormat::Bgrx, 1, 2, stride, offset, &bytes).unwrap();
            assert_eq!(frame.rgba(), &[30, 20, 10, 255, 60, 50, 40, 255]);
        }
    }
}
