// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{CameraBackend, CameraFrames, CameraSession, LumaFrame, frame_buffer::FrameWriter};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    num::NonZeroUsize,
    os::fd::AsFd,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use vauchi_contract::{CameraPurpose, Command, executor::InboxLimits};

/// Entry point for separately packaged camera providers. Only fixed native FDs cross this boundary.
pub fn run(backend: Arc<dyn CameraBackend>, available: impl FnOnce() -> bool) -> ! {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let status = if args == ["--probe"] {
        i32::from(!available())
    } else {
        execute(backend, &args).map_or(1, |_| 0)
    };
    std::process::exit(status)
}

fn execute(backend: Arc<dyn CameraBackend>, args: &[String]) -> io::Result<()> {
    if args.len() != 2 || args[0] != "--capture" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let parent: i32 = args[1].parse().map_err(|_| io::ErrorKind::InvalidInput)?;
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))?;
    if rustix::process::getppid().is_none_or(|pid| pid.as_raw_nonzero().get() != parent) {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    let writer = Arc::new(FrameWriter::attach(File::from(
        io::stdin().as_fd().try_clone_to_owned()?,
    ))?);
    let mut control = BufReader::new(File::from(io::stdout().as_fd().try_clone_to_owned()?));
    let mut request = Vec::new();
    (&mut control).take(4097).read_until(b'\n', &mut request)?;
    if request.len() > 4096 || request.last() != Some(&b'\n') {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let Command::CameraCapture {
        preview_id,
        purpose: CameraPurpose::QrScan,
    } = serde_json::from_slice(&request)?
    else {
        return Err(io::ErrorKind::InvalidData.into());
    };
    if preview_id.is_empty() || preview_id.len() > 128 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("camera-control".into())
        .spawn(move || {
            let _ = control.read(&mut [0]);
            let _ = cancel.send(());
        })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let limits = InboxLimits {
            operations: NonZeroUsize::new(1).unwrap(),
            events: NonZeroUsize::new(4).unwrap(),
            event_bytes: NonZeroUsize::new(65536).unwrap(),
        };
        let mut camera = CameraSession::with_backend(
            Arc::new(PreviewBackend { backend, writer }),
            runtime.handle().clone(),
            limits,
        )
        .map_err(io::Error::other)?;
        camera
            .capture(preview_id.clone(), CameraPurpose::QrScan)
            .map_err(io::Error::other)?;
        let result = tokio::select! {
            _ = cancelled => Ok(()),
            event = camera.next_event() => {
                let event = event.ok_or(io::ErrorKind::UnexpectedEof)?;
                let bytes = serde_json::to_vec(&event)?;
                if bytes.len() > 65535 { return Err(io::ErrorKind::InvalidData.into()); }
                let mut output = io::stdout().lock();
                output.write_all(&bytes).and_then(|_| output.write_all(b"\n"))
            }
        };
        camera.stop(&preview_id);
        camera.shutdown().await.map_err(io::Error::other)?;
        result
    });
    runtime.shutdown_timeout(Duration::from_millis(300));
    result
}

struct PreviewBackend {
    backend: Arc<dyn CameraBackend>,
    writer: Arc<FrameWriter>,
}
struct PreviewFrames {
    frames: Box<dyn CameraFrames>,
    writer: Arc<FrameWriter>,
}

impl CameraBackend for PreviewBackend {
    fn open(&self, cancelled: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
        // The optional-provider path renders the prepared FD; no second PipeWire preview source is needed.
        let frames = self.backend.open(cancelled)?;
        Ok(Box::new(PreviewFrames {
            frames,
            writer: self.writer.clone(),
        }))
    }
}

impl CameraFrames for PreviewFrames {
    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String> {
        let frame = self.frames.next_luma()?;
        if frame.is_some() {
            let preview = self
                .frames
                .preview_frame()
                .ok_or("camera provider has no live preview")?;
            self.writer
                .write(&preview)
                .map_err(|_| "camera preview resource failed")?;
        }
        Ok(frame)
    }
}
