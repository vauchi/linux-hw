// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::camera_frame::{CameraFrame, PixelFormat};
use std::{
    fs::{File, TryLockError},
    io::{self, ErrorKind},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::FileExt,
    },
};

const HEADER: usize = 32;
const CAPACITY: u64 = HEADER as u64 + 1920 * 1080 * 4;
const MAGIC: &[u8; 8] = b"VFRAME01";

#[derive(Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    bytes: u32,
    state: u32,
    generation: u64,
}

impl Header {
    fn empty(state: u32, generation: u64) -> Self {
        Self {
            width: 0,
            height: 0,
            bytes: 0,
            state,
            generation,
        }
    }

    fn read(file: &File) -> io::Result<Self> {
        let mut bytes = [0; HEADER];
        file.read_exact_at(&mut bytes, 0)?;
        if &bytes[..8] != MAGIC {
            return Err(invalid());
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = Self {
            width: word(8),
            height: word(12),
            bytes: word(16),
            state: word(20),
            generation: u64::from_le_bytes(bytes[24..].try_into().unwrap()),
        };
        match header.state {
            0 | 1 if header.width == 0 && header.height == 0 && header.bytes == 0 => {}
            2 if header.width > 0
                && header.width <= 1920
                && header.height > 0
                && header.height <= 1080
                && header.bytes == header.width * header.height * 4
                && header.generation > 0 => {}
            _ => return Err(invalid()),
        }
        Ok(header)
    }

    fn write(self, file: &File) -> io::Result<()> {
        let mut bytes = [0; HEADER];
        bytes[..8].copy_from_slice(MAGIC);
        for (offset, word) in [
            (8, self.width),
            (12, self.height),
            (16, self.bytes),
            (20, self.state),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        bytes[24..].copy_from_slice(&self.generation.to_le_bytes());
        file.write_all_at(&bytes, 0)
    }
}

struct Lock<'a>(&'a File);
impl Drop for Lock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn try_lock(file: &File, exclusive: bool) -> io::Result<Option<Lock<'_>>> {
    match if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    } {
        Ok(()) => Ok(Some(Lock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn invalid() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "invalid native preview buffer")
}

fn validate_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != CAPACITY {
        return Err(invalid());
    }
    Ok(())
}

/// A bounded native preview resource. Only the capture owner writes frames.
pub struct FrameWriter {
    file: File,
}

impl FrameWriter {
    /// Initialize a new, empty file supplied by the native resource owner.
    pub fn initialize(file: File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(invalid());
        }
        file.set_len(CAPACITY)?;
        Header::empty(1, 0).write(&file)?;
        Ok(Self { file })
    }

    pub fn reader_fd(&self) -> io::Result<OwnedFd> {
        // flock belongs to the open file description; dup would share the writer's lock.
        File::open(format!("/proc/self/fd/{}", self.file.as_raw_fd())).map(Into::into)
    }

    pub fn reader(&self) -> io::Result<FrameReader> {
        FrameReader::new(File::from(self.reader_fd()?))
    }

    /// Drop a preview update rather than wait for a busy renderer.
    pub fn write(&self, frame: &CameraFrame) -> io::Result<bool> {
        let Some(_lock) = try_lock(&self.file, true)? else {
            return Ok(false);
        };
        let previous = Header::read(&self.file)?;
        if previous.state == 0 {
            return Err(io::Error::new(
                ErrorKind::BrokenPipe,
                "native preview retired",
            ));
        }
        let generation = previous.generation.checked_add(1).ok_or_else(invalid)?;
        self.file.write_all_at(frame.rgba(), HEADER as u64)?;
        Header {
            width: frame.width(),
            height: frame.height(),
            bytes: frame.rgba().len() as u32,
            state: 2,
            generation,
        }
        .write(&self.file)?;
        Ok(true)
    }

    /// Retire from the capture worker; existing reader descriptors observe closure.
    pub fn close(&self) -> io::Result<()> {
        self.file.lock()?;
        let _lock = Lock(&self.file);
        Header::empty(0, 0).write(&self.file)
    }
}

impl Drop for FrameWriter {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub struct FrameReader {
    file: File,
    generation: u64,
    closed: bool,
}

impl FrameReader {
    pub fn new(file: File) -> io::Result<Self> {
        validate_file(&file)?;
        Ok(Self {
            file,
            generation: 0,
            closed: false,
        })
    }

    pub fn poll(&mut self) -> io::Result<Option<CameraFrame>> {
        if self.closed {
            return Ok(None);
        }
        let Some(_lock) = try_lock(&self.file, false)? else {
            return Ok(None);
        };
        validate_file(&self.file)?;
        let header = Header::read(&self.file)?;
        if header.state == 0 {
            self.closed = true;
        }
        if header.state != 2 || header.generation == self.generation {
            return Ok(None);
        }
        let mut pixels = vec![0; header.bytes as usize];
        self.file.read_exact_at(&mut pixels, HEADER as u64)?;
        let frame = CameraFrame::from_raw(
            PixelFormat::Rgba,
            header.width,
            header.height,
            header.width as usize * 4,
            0,
            &pixels,
        )
        .map_err(|_| invalid())?;
        self.generation = header.generation;
        Ok(Some(frame))
    }

    pub fn closed(&self) -> bool {
        self.closed
    }
}
