// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::frame_directory::FrameDirectory;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod capture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    Auto,
    PipeWire,
    Direct,
    Disabled,
}

impl FromStr for BackendChoice {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "pipewire" => Ok(Self::PipeWire),
            "direct" => Ok(Self::Direct),
            "disabled" => Ok(Self::Disabled),
            _ => Err("camera backend must be auto, pipewire, direct or disabled"),
        }
    }
}

/// Optional, separately packaged native providers. No loader search through PATH.
#[derive(Clone)]
pub struct CameraModules {
    path: PathBuf,
    choice: BackendChoice,
    directory: FrameDirectory,
    serial: Arc<Mutex<()>>,
}

impl CameraModules {
    pub fn discover() -> Option<Self> {
        let choice = std::env::var("VAUCHI_CAMERA_BACKEND")
            .unwrap_or_else(|_| "auto".into())
            .parse()
            .ok()?;
        let executable = std::env::current_exe().ok()?;
        let folder = executable.parent()?;
        let sandbox = Path::new("/.flatpak-info").exists();
        Self::discover_in(folder, choice, sandbox).or_else(|| {
            let prefix = folder.parent()?;
            Self::discover_in(&prefix.join("libexec/vauchi"), choice, sandbox)
        })
    }

    pub fn discover_in(folder: &Path, choice: BackendChoice, sandbox: bool) -> Option<Self> {
        let choices: &[BackendChoice] = match choice {
            BackendChoice::Auto => &[BackendChoice::PipeWire, BackendChoice::Direct],
            BackendChoice::PipeWire => &[BackendChoice::PipeWire],
            BackendChoice::Direct => &[BackendChoice::Direct],
            BackendChoice::Disabled => &[],
        };
        choices.iter().find_map(|&choice| {
            if sandbox && choice == BackendChoice::Direct {
                return None;
            }
            let name = match choice {
                BackendChoice::PipeWire => "vauchi-camera-pipewire",
                BackendChoice::Direct => "vauchi-camera-v4l2",
                _ => return None,
            };
            let path = folder.join(name);
            use std::os::unix::fs::PermissionsExt;
            let metadata = path.metadata().ok()?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }
            Some(Self {
                path,
                choice,
                directory: FrameDirectory::default(),
                serial: Arc::default(),
            })
        })
    }

    pub fn choice(&self) -> BackendChoice {
        self.choice
    }
    pub fn directory(&self) -> FrameDirectory {
        self.directory.clone()
    }

    /// Presence probes never start capture or ask the portal for permission.
    pub fn available(&self) -> bool {
        let Ok(mut child) = Command::new(&self.path)
            .arg("--probe")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if started.elapsed() < Duration::from_secs(2) => {
                    std::thread::park_timeout(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }
}
