// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

mod capture;
mod formats;

use crate::ports::{CameraBackend, CameraFrames};
use pipewire as pw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct PipeWireCamera;

impl PipeWireCamera {
    /// Both display libraries share the app process, but not camera ownership.
    /// The process scope prevents one app from displaying another's capture.
    pub fn preview_node_name(preview_id: &str) -> Result<String, &'static str> {
        if preview_id.is_empty() || preview_id.len() > 128 {
            return Err("invalid camera preview identity");
        }
        let identity: String = preview_id
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(format!("vauchi.camera.{}.{identity}", std::process::id()))
    }

    pub fn available() -> bool {
        camera_connection().is_ok()
    }
}

impl CameraBackend for PipeWireCamera {
    fn open(&self, cancelled: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        self.open_preview("capture", cancelled)
    }

    fn open_preview(
        &self,
        preview_id: &str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Box<dyn CameraFrames>, String> {
        let name = Self::preview_node_name(preview_id)?;
        let (mainloop, core, target) = camera_connection()?;
        Ok(Box::new(capture::Capture::new(
            mainloop, core, target, name, cancelled,
        )?))
    }
}

fn camera_connection() -> Result<(pw::main_loop::MainLoopRc, pw::core::CoreRc, String), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|_| "camera loop unavailable")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|_| "camera context unavailable")?;
    let core = context
        .connect_rc(None)
        .map_err(|_| "camera service unavailable or permission denied")?;
    let target = find_camera(&mainloop, &core)?;
    Ok((mainloop, core, target))
}

fn find_camera(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
) -> Result<String, String> {
    let registry = core
        .get_registry()
        .map_err(|_| "camera registry unavailable")?;
    let found = Rc::new(RefCell::new(None));
    let result = found.clone();
    let _listener = registry
        .add_listener_local()
        .global(move |object| {
            let Some(props) = object.props else {
                return;
            };
            let Some(name) = props.get("node.name") else {
                return;
            };
            if props.get("media.class") == Some("Video/Source")
                && !name.starts_with("vauchi.camera.")
                && result.borrow().is_none()
            {
                *result.borrow_mut() = Some(props.get("object.serial").unwrap_or(name).to_owned());
            }
        })
        .register();
    let start = Instant::now();
    while found.borrow().is_none() && start.elapsed() < Duration::from_secs(1) {
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(25)));
    }
    found
        .take()
        .ok_or_else(|| "no PipeWire camera is available".into())
}
