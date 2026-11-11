// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Desktop notifications (ADR-077: `PostNotification` is a hardware
//! command; private#1029). Core prepares each notification; the session
//! shows it and reports a click as `DeepLinkOpened`.
//!
//! Not built on `Executor`: a new request there cancels the previous one,
//! and a second notification must not stop the clicks on the first. One
//! listener runs for the session's life; each post runs on its own.

use crate::ports::Notifier;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vauchi_contract::{Event, NotificationSpec};

const TRANSPORT: &str = "notification";
/// Clicks and failures are rare; a full inbox drops the newest rather than
/// growing without bound.
const INBOX: usize = 32;

pub struct NotificationSession {
    backend: Arc<dyn Notifier>,
    runtime: Handle,
    sender: mpsc::Sender<Event>,
    inbox: mpsc::Receiver<Event>,
    tasks: Vec<JoinHandle<()>>,
}

impl NotificationSession {
    pub fn with_backend(backend: Arc<dyn Notifier>, runtime: Handle) -> Self {
        let (sender, inbox) = mpsc::channel(INBOX);
        let listener = {
            let backend = backend.clone();
            let sender = sender.clone();
            runtime.spawn(async move {
                let (opened, mut links) = mpsc::channel::<String>(INBOX);
                let listen = backend.activations(opened);
                let forward = async {
                    while let Some(uri) = links.recv().await {
                        let _ = sender.try_send(Event::DeepLinkOpened { uri });
                    }
                };
                // No request waits on the listener: a service that is missing
                // shows up as a HardwareError on the next post instead.
                let (_listened, ()) = tokio::join!(listen, forward);
            })
        };
        Self {
            backend,
            runtime,
            sender,
            inbox,
            tasks: vec![listener],
        }
    }

    /// `PostNotification`: show Core's prepared notification.
    pub fn post(&mut self, notification: NotificationSpec) {
        self.tasks.retain(|task| !task.is_finished());
        let backend = self.backend.clone();
        let sender = self.sender.clone();
        self.tasks.push(self.runtime.spawn(async move {
            if let Err(error) = backend.post(notification).await {
                let _ = sender.try_send(Event::HardwareError {
                    transport: TRANSPORT.into(),
                    error,
                });
            }
        }));
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.inbox.try_recv().ok()
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.inbox.recv().await
    }

    pub fn shutdown(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

impl Drop for NotificationSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}
