// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The V4L2 camera behind the camera port: the first device at its highest
//! frame rate, each frame decoded to RGB and packed to luma. Use Nokhwa's
//! Linux backend directly to avoid its wrapper's unmaintained paste macro.
use crate::camera::rgb_to_luma;
use crate::ports::{CameraBackend, CameraFrames, LumaFrame};
use nokhwa_bindings_linux::V4LCaptureDevice;
use nokhwa_core::pixel_format::RgbFormat;
use nokhwa_core::traits::CaptureBackendTrait;
use nokhwa_core::types::{CameraIndex, RequestedFormat, RequestedFormatType};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct NokhwaCamera;

struct NokhwaFrames {
    camera: V4LCaptureDevice<'static>,
    cancelled: Arc<AtomicBool>,
}

impl CameraFrames for NokhwaFrames {
    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String> {
        while !self.cancelled.load(Ordering::SeqCst) {
            // A frame that fails to arrive or decode is skipped, as the
            // GTK loop did; the next one is tried.
            let Ok(frame) = self.camera.frame() else {
                continue;
            };
            let Ok(decoded) = frame.decode_image::<RgbFormat>() else {
                continue;
            };
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
        let requested =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
        let mut camera = V4LCaptureDevice::new(&CameraIndex::Index(0), requested)
            .map_err(|e| format!("Camera open failed: {e}"))?;
        camera
            .open_stream()
            .map_err(|e| format!("Camera stream failed: {e}"))?;
        Ok(Box::new(NokhwaFrames { camera, cancelled }))
    }
}
