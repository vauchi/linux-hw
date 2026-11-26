// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::frame_buffer::FrameWriter;
use std::{
    io,
    os::fd::OwnedFd,
    sync::{Arc, Mutex},
};

pub enum FrameResource {
    Absent,
    Unchanged,
    New { generation: u64, fd: OwnedFd },
}

struct Entry {
    id: String,
    generation: u64,
    writer: Arc<FrameWriter>,
}

#[derive(Default)]
struct State {
    generation: u64,
    active: Option<Entry>,
}

/// An app-local registry of prepared native surfaces; IDs never identify devices.
#[derive(Clone, Default)]
pub struct FrameDirectory(Arc<Mutex<State>>);

impl FrameDirectory {
    pub fn publish(&self, id: &str, writer: Arc<FrameWriter>) -> io::Result<FrameLease> {
        if id.is_empty() || id.len() > 128 {
            return Err(invalid());
        }
        let mut state = self.0.lock().map_err(|_| invalid())?;
        if state
            .active
            .as_ref()
            .is_some_and(|entry| Arc::ptr_eq(&entry.writer, &writer))
        {
            return Err(invalid());
        }
        let generation = state.generation.checked_add(1).ok_or_else(invalid)?;
        state.generation = generation;
        let old = state.active.replace(Entry {
            id: id.into(),
            generation,
            writer,
        });
        drop(state);
        if let Some(old) = old {
            old.writer.close()?;
        }
        Ok(FrameLease {
            directory: self.clone(),
            generation,
        })
    }

    /// An unchanged generation avoids opening an FD on every display refresh.
    pub fn poll(&self, id: &str, previous: u64) -> io::Result<FrameResource> {
        let state = self.0.lock().map_err(|_| invalid())?;
        let Some(entry) = state.active.as_ref().filter(|entry| entry.id == id) else {
            return Ok(FrameResource::Absent);
        };
        if entry.generation == previous {
            return Ok(FrameResource::Unchanged);
        }
        let generation = entry.generation;
        let writer = entry.writer.clone();
        drop(state);
        let fd = writer.reader_fd()?;
        let state = self.0.lock().map_err(|_| invalid())?;
        if state
            .active
            .as_ref()
            .is_none_or(|entry| entry.generation != generation)
        {
            return Ok(FrameResource::Absent);
        }
        Ok(FrameResource::New { generation, fd })
    }
}

pub struct FrameLease {
    directory: FrameDirectory,
    generation: u64,
}

impl Drop for FrameLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.directory.0.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|entry| entry.generation == self.generation)
        {
            let entry = state.active.take().unwrap();
            drop(state);
            let _ = entry.writer.close();
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid native preview resource",
    )
}
