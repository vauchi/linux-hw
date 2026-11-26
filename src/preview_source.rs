// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

mod resolver;
pub use resolver::PreviewResolver;

pub type RemoteOpener = Arc<dyn Fn() -> Result<OwnedFd, String> + Send + Sync>;

/// Native resources passed by app wiring, never by Core's frame protocol.
#[derive(Debug)]
pub struct PreviewSource {
    pub node_id: u32,
    pub serial: u64,
    pub remote_fd: Option<OwnedFd>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("invalid camera preview source")]
    Invalid,
    #[error("camera preview directory unavailable")]
    Unavailable,
    #[error("{0}")]
    Remote(String),
}

#[derive(Clone)]
struct Entry {
    preview_id: String,
    node_id: u32,
    serial: u64,
    generation: u64,
    remote: Option<RemoteOpener>,
}

#[derive(Default)]
struct State {
    generation: u64,
    active: Option<Entry>,
}

/// One current capture per hardware instance, isolated from other apps.
#[derive(Clone, Default)]
pub struct PreviewDirectory(Arc<Mutex<State>>);

pub struct PreviewLease {
    directory: PreviewDirectory,
    generation: u64,
}

impl PreviewDirectory {
    pub fn publish(
        &self,
        preview_id: &str,
        node_id: u32,
        serial: u64,
        remote: Option<RemoteOpener>,
    ) -> Result<PreviewLease, SourceError> {
        if preview_id.is_empty() || preview_id.len() > 128 || node_id == u32::MAX || serial == 0 {
            return Err(SourceError::Invalid);
        }
        let mut state = self.0.lock().map_err(|_| SourceError::Unavailable)?;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(SourceError::Invalid)?;
        let generation = state.generation;
        state.active = Some(Entry {
            preview_id: preview_id.into(),
            node_id,
            serial,
            generation,
            remote,
        });
        Ok(PreviewLease {
            directory: self.clone(),
            generation,
        })
    }

    pub fn resolve(&self, preview_id: &str) -> Result<Option<PreviewSource>, SourceError> {
        let entry = self
            .0
            .lock()
            .map_err(|_| SourceError::Unavailable)?
            .active
            .clone();
        let Some(entry) = entry.filter(|entry| entry.preview_id == preview_id) else {
            return Ok(None);
        };
        // A portal remote is a protocol connection, so each sink needs a fresh FD.
        // Opening it must not hold the directory lock or block capture retirement.
        let remote_fd = entry
            .remote
            .as_ref()
            .map(|open| open())
            .transpose()
            .map_err(SourceError::Remote)?;
        let state = self.0.lock().map_err(|_| SourceError::Unavailable)?;
        if state
            .active
            .as_ref()
            .is_none_or(|current| current.generation != entry.generation)
        {
            return Ok(None);
        }
        Ok(Some(PreviewSource {
            node_id: entry.node_id,
            serial: entry.serial,
            remote_fd,
        }))
    }
}

impl Drop for PreviewLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.directory.0.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|entry| entry.generation == self.generation)
        {
            state.active = None;
        }
    }
}
