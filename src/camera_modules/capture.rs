// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::CameraModules;
use crate::frame_buffer::FrameWriter;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use vauchi_contract::{CameraPurpose, Event};

impl CameraModules {
    pub(crate) fn capture(
        &self,
        id: String,
        purpose: CameraPurpose,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<Event>, String> {
        // The executor can cancel its future before the native worker has released the device.
        let _serial = self
            .serial
            .lock()
            .map_err(|_| "camera worker unavailable")?;
        if cancelled.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if purpose != CameraPurpose::QrScan {
            return Err("unsupported camera purpose".into());
        }
        self.capture_inner(id, purpose, cancelled)
            .map_err(|_| "camera provider failed".into())
    }

    fn capture_inner(
        &self,
        id: String,
        purpose: CameraPurpose,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Option<Event>> {
        use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
        let file = File::from(memfd_create(
            c"vauchi-preview",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?);
        let writer = Arc::new(FrameWriter::initialize(file.try_clone()?)?);
        fcntl_add_seals(&file, SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL)?;
        let _lease = self.directory.publish(&id, writer.clone())?;
        let (mut stream, child_stream) = UnixStream::pair()?;
        let mut process = Process {
            child: Command::new(&self.path)
                .arg("--capture")
                .arg(std::process::id().to_string())
                .stdin(Stdio::from(writer.writer_fd()?))
                .stdout(Stdio::from(OwnedFd::from(child_stream)))
                .stderr(Stdio::null())
                .spawn()?,
            control: stream.try_clone()?,
        };
        let request = vauchi_contract::Command::CameraCapture {
            preview_id: id,
            purpose,
        };
        stream.write_all(&serde_json::to_vec(&request)?)?;
        stream.write_all(b"\n")?;
        stream.set_nonblocking(true)?;
        let mut message = Vec::new();
        let mut chunk = [0; 4096];
        while !cancelled.load(Ordering::SeqCst) {
            match stream.read(&mut chunk) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => {
                    if message.len() + count > 65536 {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    message.extend_from_slice(&chunk[..count]);
                    if message.last() == Some(&b'\n') {
                        let event: Event = serde_json::from_slice(&message)?;
                        return match event {
                            Event::QrScanned { .. } | Event::HardwareError { .. }
                                if camera_event(&event) =>
                            {
                                Ok(Some(event))
                            }
                            _ => Err(io::ErrorKind::InvalidData.into()),
                        };
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if process.child.try_wait()?.is_some() {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    std::thread::park_timeout(Duration::from_millis(10));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }
}

fn camera_event(event: &Event) -> bool {
    match event {
        Event::QrScanned { data } => data.len() <= 32768,
        Event::HardwareError { transport, error } => transport == "Camera" && error.len() <= 4096,
        _ => false,
    }
}

struct Process {
    child: Child,
    control: UnixStream,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.control.write_all(b"x");
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(500) {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::park_timeout(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
