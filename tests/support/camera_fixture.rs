// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use vauchi_linux_hw::{
    CameraBackend, CameraFrames, LumaFrame,
    camera_frame::{CameraFrame, PixelFormat},
};

struct Camera {
    mode: String,
    folder: std::path::PathBuf,
}
struct Frames {
    cancelled: Arc<AtomicBool>,
    frame: CameraFrame,
    sent: bool,
    mode: String,
}

impl CameraBackend for Camera {
    fn open(&self, cancelled: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        if self.mode == "denied" {
            return Err("camera permission denied".into());
        }
        let input = std::fs::read(self.folder.join("fixture-rgb")).unwrap();
        let side = u32::from_le_bytes(input[..4].try_into().unwrap());
        let frame = CameraFrame::from_raw(
            PixelFormat::Rgb,
            side,
            side,
            side as usize * 3,
            0,
            &input[4..],
        )
        .unwrap();
        Ok(Box::new(Frames {
            cancelled,
            frame,
            sent: false,
            mode: self.mode.clone(),
        }))
    }
}

impl CameraFrames for Frames {
    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String> {
        if self.mode == "live" && !self.cancelled.load(Ordering::SeqCst) {
            return Ok(Some(LumaFrame {
                luma: vec![128; 2],
                width: 2,
                height: 1,
            }));
        }
        if self.cancelled.load(Ordering::SeqCst) || self.sent {
            return Ok(None);
        }
        self.sent = true;
        Ok(Some(LumaFrame {
            luma: self.frame.luma(),
            width: self.frame.width(),
            height: self.frame.height(),
        }))
    }
    fn preview_frame(&self) -> Option<CameraFrame> {
        if self.mode == "blind" {
            return None;
        }
        Some(self.frame.clone())
    }
}

fn main() {
    let invoked = std::path::PathBuf::from(std::env::args().next().unwrap());
    let mode =
        std::fs::read_to_string(invoked.parent().unwrap().join("fixture-mode")).unwrap_or_default();
    if mode == "invalid" && std::env::args().nth(1).as_deref() == Some("--capture") {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        out.write_all(&serde_json::to_vec(&vauchi_contract::Event::WakeupFired).unwrap())
            .unwrap();
        out.write_all(b"\n").unwrap();
        return;
    }
    vauchi_linux_hw::camera_helper::run(
        Arc::new(Camera {
            mode,
            folder: invoked.parent().unwrap().into(),
        }),
        || true,
    )
}
