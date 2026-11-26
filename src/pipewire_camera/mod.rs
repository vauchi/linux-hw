// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

mod capture;
mod formats;
mod portal_runtime;
mod publication;

use crate::ports::{CameraBackend, CameraFrames};
use pipewire as pw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct PipeWireCamera {
    portal: Option<crate::camera_portal::CameraPortal>,
    portal_required: bool,
    directory: crate::preview_source::PreviewDirectory,
}

impl PipeWireCamera {
    /// Diagnostic name; the app-owned directory authorizes preview consumers.
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
        match portal_runtime::portal(None, portal_runtime::sandboxed()) {
            Ok(Some(_)) => true,
            Ok(None) => camera_connection(None).is_ok(),
            Err(_) => false,
        }
    }

    pub fn with_portal(portal: crate::camera_portal::CameraPortal, required: bool) -> Self {
        Self {
            portal: Some(portal),
            portal_required: required,
            ..Self::default()
        }
    }

    pub fn preview_directory(&self) -> crate::preview_source::PreviewDirectory {
        self.directory.clone()
    }
}

impl CameraBackend for PipeWireCamera {
    fn preview_directory(&self) -> Option<crate::preview_source::PreviewDirectory> {
        Some(self.directory.clone())
    }

    fn open(&self, cancelled: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        self.open_preview("capture", cancelled)
    }

    fn open_preview(
        &self,
        preview_id: &str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Box<dyn CameraFrames>, String> {
        let name = Self::preview_node_name(preview_id)?;
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("camera request cancelled".into());
        }
        let access = portal_runtime::authorize(
            self.portal.clone(),
            self.portal_required || portal_runtime::sandboxed(),
            cancelled.clone(),
        )
        .map_err(|error| error.to_string())?;
        let (mainloop, core, target) = camera_connection(access.fd)?;
        let publication = publication::Publication::new(
            self.directory.clone(),
            preview_id.to_owned(),
            access.remote,
        );
        Ok(Box::new(capture::Capture::new(
            mainloop,
            core,
            target,
            name,
            cancelled,
            publication,
        )?))
    }
}

fn camera_connection(
    fd: Option<std::os::fd::OwnedFd>,
) -> Result<(pw::main_loop::MainLoopRc, pw::core::CoreRc, String), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|_| "camera loop unavailable")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|_| "camera context unavailable")?;
    let core = match fd {
        Some(fd) => context.connect_fd_rc(fd, None),
        None => context.connect_rc(None),
    }
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
