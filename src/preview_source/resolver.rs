// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{PreviewDirectory, PreviewSource, SourceError};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

type Reply = (String, Result<Option<PreviewSource>, SourceError>);

/// A GUI-thread poll never performs portal I/O. At most one lookup is pending.
pub struct PreviewResolver {
    requests: SyncSender<String>,
    replies: Receiver<Reply>,
    pending: bool,
}

impl PreviewDirectory {
    pub fn resolver(&self) -> Result<PreviewResolver, SourceError> {
        let (requests, incoming) = mpsc::sync_channel::<String>(1);
        let (outgoing, replies) = mpsc::sync_channel(1);
        let directory = self.clone();
        std::thread::Builder::new()
            .name("vauchi-preview-remote".into())
            .spawn(move || {
                while let Ok(id) = incoming.recv() {
                    let result = directory.resolve(&id);
                    if outgoing.send((id, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| SourceError::Unavailable)?;
        Ok(PreviewResolver {
            requests,
            replies,
            pending: false,
        })
    }
}

impl PreviewResolver {
    pub fn poll(&mut self, preview_id: &str) -> Result<Option<PreviewSource>, SourceError> {
        if preview_id.is_empty() || preview_id.len() > 128 {
            return Err(SourceError::Invalid);
        }
        match self.replies.try_recv() {
            Ok((id, result)) => {
                self.pending = false;
                if id == preview_id {
                    return result;
                }
            }
            Err(TryRecvError::Disconnected) => return Err(SourceError::Unavailable),
            Err(TryRecvError::Empty) => {}
        }
        if !self.pending {
            self.requests
                .try_send(preview_id.to_owned())
                .map_err(|_| SourceError::Unavailable)?;
            self.pending = true;
        }
        Ok(None)
    }
}
