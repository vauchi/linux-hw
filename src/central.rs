// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::executor::Executor;
use crate::ports::{Central, ExecutorError, HardwareFailure, LinkRequest, LinkRequests};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, InboxLimits};

/// The longest BlueZ device id is a 17-character address; this leaves
/// room for other OS identifiers without accepting arbitrary input.
const MAX_DEVICE_ID: usize = 128;
const MAX_UUID: usize = 128;
/// The longest attribute value ATT can carry.
const MAX_VALUE: usize = 512;
const MAX_QUEUED_REQUESTS: usize = 64;

#[derive(Clone)]
struct Link {
    device_id: String,
    // The watch channel hands out clones of the request; the receiver is
    // taken by the one operation that runs it.
    requests: Arc<Mutex<Option<mpsc::Receiver<LinkRequest>>>>,
}

/// Host-owned central executor: one outbound link at a time, kept until
/// Core disconnects it or connects to another peer.
pub struct BleCentral {
    executor: Executor<Link>,
    link: Option<(String, mpsc::Sender<LinkRequest>)>,
}

impl BleCentral {
    pub fn with_backend(
        backend: Arc<dyn Central>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let native = move |link: Link, events| {
            let backend = backend.clone();
            Box::pin(async move {
                let requests = link
                    .requests
                    .lock()
                    .ok()
                    .and_then(|mut receiver| receiver.take())
                    .ok_or(HardwareFailure::Failed)?;
                backend
                    .connect(link.device_id, LinkRequests(requests), events)
                    .await
            }) as _
        };
        Ok(Self {
            executor: Executor::new(native, &runtime, limits)?,
            link: None,
        })
    }

    pub fn connect(&mut self, device_id: String) -> Result<(), DeliveryError> {
        self.executor.ensure_open()?;
        if device_id.len() > MAX_DEVICE_ID {
            return Err(DeliveryError::PayloadTooLarge);
        }
        let (sender, receiver) = mpsc::channel(MAX_QUEUED_REQUESTS);
        self.executor.start(Link {
            device_id: device_id.clone(),
            requests: Arc::new(Mutex::new(Some(receiver))),
        })?;
        self.link = Some((device_id, sender));
        Ok(())
    }

    /// Queue a write or read for the link to `device_id`. `Full` asks the
    /// host to drain events and retry, as for [`crate::BleAdvertiser::notify`].
    pub fn request(&mut self, device_id: &str, request: LinkRequest) -> Result<(), DeliveryError> {
        let (uuid, data) = match &request {
            LinkRequest::Write { uuid, data } => (uuid, data.len()),
            LinkRequest::Read { uuid } => (uuid, 0),
        };
        if uuid.len() > MAX_UUID || data > MAX_VALUE {
            return Err(DeliveryError::PayloadTooLarge);
        }
        let sender = self.sender_for(device_id)?;
        sender.try_send(request).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => DeliveryError::Full,
            mpsc::error::TrySendError::Closed(_) => DeliveryError::Inactive,
        })
    }

    pub fn disconnect(&mut self, device_id: &str) -> Result<(), DeliveryError> {
        self.sender_for(device_id)?;
        self.link = None;
        self.executor.stop();
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.executor.pop()
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.executor.next_event().await
    }

    /// Close delivery, drop the link and wait for its future to be dropped.
    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.link = None;
        self.executor.shutdown().await
    }

    fn sender_for(&self, device_id: &str) -> Result<&mpsc::Sender<LinkRequest>, DeliveryError> {
        match &self.link {
            Some((linked, sender)) if linked == device_id => Ok(sender),
            _ => Err(DeliveryError::Inactive),
        }
    }
}
