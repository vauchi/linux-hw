// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::camera::rgb_to_luma;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb,
    Rgbx,
    Rgba,
    Bgrx,
    Yuy2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("unsupported camera frame dimensions")]
    Dimensions,
    #[error("invalid camera frame stride")]
    Stride,
    #[error("incomplete camera frame buffer")]
    Buffer,
}

/// One validated frame shared by the preview publisher and QR decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl CameraFrame {
    pub fn from_raw(
        format: PixelFormat,
        width: u32,
        height: u32,
        stride: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 || width > 1920 || height > 1080 {
            return Err(FrameError::Dimensions);
        }
        if format == PixelFormat::Yuy2 && !width.is_multiple_of(2) {
            return Err(FrameError::Dimensions);
        }
        let bytes_per_pixel = match format {
            PixelFormat::Rgb => 3,
            PixelFormat::Yuy2 => 2,
            _ => 4,
        };
        let row_bytes = width as usize * bytes_per_pixel;
        if stride < row_bytes {
            return Err(FrameError::Stride);
        }
        let end = stride
            .checked_mul(height as usize - 1)
            .and_then(|last_row| last_row.checked_add(row_bytes))
            .and_then(|extent| extent.checked_add(offset))
            .ok_or(FrameError::Buffer)?;
        if end > bytes.len() {
            return Err(FrameError::Buffer);
        }
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height as usize {
            let start = offset + row * stride;
            let pixels = &bytes[start..start + row_bytes];
            if format == PixelFormat::Yuy2 {
                for pair in pixels.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&yuv(pair[0], pair[1], pair[3]));
                    rgba.extend_from_slice(&yuv(pair[2], pair[1], pair[3]));
                }
            } else {
                for pixel in pixels.chunks_exact(bytes_per_pixel) {
                    let [r, g, b] = if format == PixelFormat::Bgrx {
                        [pixel[2], pixel[1], pixel[0]]
                    } else {
                        [pixel[0], pixel[1], pixel[2]]
                    };
                    rgba.extend_from_slice(&[r, g, b, 255]);
                }
            }
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn luma(&self) -> Vec<u8> {
        let rgb: Vec<u8> = self
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| p[..3].iter().copied())
            .collect();
        rgb_to_luma(&rgb)
    }
}

fn yuv(y: u8, u: u8, v: u8) -> [u8; 4] {
    let y = (i32::from(y) - 16).max(0);
    let u = i32::from(u) - 128;
    let v = i32::from(v) - 128;
    let channel = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    [
        channel(298 * y + 409 * v),
        channel(298 * y - 100 * u - 208 * v),
        channel(298 * y + 516 * u),
        255,
    ]
}
