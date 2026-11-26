// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The V4L2 camera behind the camera port: bounded RGB preview and luma
//! from the same decoded device frame. Use Nokhwa's
//! Linux backend directly to avoid its wrapper's unmaintained paste macro.
use crate::camera::rgb_to_luma;
use crate::camera_frame::{CameraFrame, PixelFormat};
use crate::ports::{CameraBackend, CameraFrames, LumaFrame};
use nokhwa_bindings_linux::V4LCaptureDevice;
use nokhwa_core::pixel_format::{FormatDecoder, RgbFormat};
use nokhwa_core::traits::CaptureBackendTrait;
use nokhwa_core::types::{
    CameraFormat, CameraIndex, RequestedFormat, RequestedFormatType, Resolution,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct NokhwaCamera;

impl NokhwaCamera {
    pub fn available() -> bool {
        !std::path::Path::new("/.flatpak-info").exists()
            && std::fs::File::options()
                .read(true)
                .write(true)
                .open("/dev/video0")
                .is_ok()
    }
}

struct NokhwaFrames {
    camera: V4LCaptureDevice<'static>,
    cancelled: Arc<AtomicBool>,
    preview: Option<CameraFrame>,
}

impl CameraFrames for NokhwaFrames {
    fn preview_frame(&self) -> Option<CameraFrame> {
        self.preview.clone()
    }

    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String> {
        while !self.cancelled.load(Ordering::SeqCst) {
            let frame = self
                .camera
                .frame()
                .map_err(|_| "direct camera stopped delivering frames")?;
            if frame.resolution().width() > 1920 || frame.resolution().height() > 1080 {
                return Err("unsupported direct camera resolution".into());
            }
            let Ok(decoded) = frame.decode_image::<RgbFormat>() else {
                continue;
            };
            self.preview = Some(
                CameraFrame::from_raw(
                    PixelFormat::Rgb,
                    decoded.width(),
                    decoded.height(),
                    decoded.width() as usize * 3,
                    0,
                    decoded.as_raw(),
                )
                .map_err(|_| "unsupported direct camera frame")?,
            );
            return Ok(Some(LumaFrame {
                luma: rgb_to_luma(decoded.as_raw()),
                width: decoded.width(),
                height: decoded.height(),
            }));
        }
        Ok(None)
    }
}

impl Drop for NokhwaFrames {
    fn drop(&mut self) {
        self.camera.stop_stream().ok();
    }
}

impl CameraBackend for NokhwaCamera {
    fn open(&self, cancelled: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        if std::path::Path::new("/.flatpak-info").exists() {
            return Err("direct camera access is unavailable in Flatpak".into());
        }
        let mut camera = RgbFormat::FORMATS
            .iter()
            .find_map(|format| {
                let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(
                    CameraFormat::new(Resolution::new(640, 480), *format, 30),
                ));
                V4LCaptureDevice::new(&CameraIndex::Index(0), requested)
                    .ok()
                    .filter(|camera| {
                        let resolution = camera.resolution();
                        resolution.width() > 0
                            && resolution.width() <= 1920
                            && resolution.height() > 0
                            && resolution.height() <= 1080
                    })
            })
            .ok_or("no accessible direct camera with a supported preview format")?;
        camera
            .open_stream()
            .map_err(|e| format!("Camera stream failed: {e}"))?;
        Ok(Box::new(NokhwaFrames {
            camera,
            cancelled,
            preview: None,
        }))
    }
}
