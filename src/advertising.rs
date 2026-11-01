// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::executor::Executor;
use crate::ports::{Advertising, ExecutorError, HardwareFailure, Notification, Outbound};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, InboxLimits};

const MAX_SERVICE_UUID: usize = 128;
/// Core's advertised token; only its first two bytes go on air.
const MAX_PAYLOAD: usize = 64;
/// The longest attribute value ATT can carry.
const MAX_VALUE: usize = 512;
const MAX_QUEUED_NOTIFICATIONS: usize = 64;

#[derive(Clone)]
struct Advertisement {
    service_uuid: String,
    payload: Vec<u8>,
    // The watch channel hands out clones of the request; the receiver is
    // taken by the one operation that runs it.
    outbound: Arc<Mutex<Option<mpsc::Receiver<Notification>>>>,
}

/// Host-owned peripheral executor: one advertisement with its GATT service
/// at a time, kept until Core stops it (`BleStopAdvertising`) or replaces
/// it. The host serializes calls the same way as for [`crate::BleScanner`].
pub struct BleAdvertiser {
    executor: Executor<Advertisement>,
    outbound: Option<mpsc::Sender<Notification>>,
}

impl BleAdvertiser {
    pub fn with_backend(
        backend: Arc<dyn Advertising>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let native = move |request: Advertisement, events| {
            let backend = backend.clone();
            Box::pin(async move {
                let outbound = request
                    .outbound
                    .lock()
                    .ok()
                    .and_then(|mut receiver| receiver.take())
                    .ok_or(HardwareFailure::Failed)?;
                backend
                    .advertise(
                        request.service_uuid,
                        request.payload,
                        Outbound(outbound),
                        events,
                    )
                    .await
            }) as _
        };
        Ok(Self {
            executor: Executor::new(native, &runtime, limits)?,
            outbound: None,
        })
    }

    pub fn start(&mut self, service_uuid: String, payload: Vec<u8>) -> Result<(), DeliveryError> {
        self.executor.ensure_open()?;
        if service_uuid.len() > MAX_SERVICE_UUID || payload.len() > MAX_PAYLOAD {
            return Err(DeliveryError::PayloadTooLarge);
        }
        let (sender, receiver) = mpsc::channel(MAX_QUEUED_NOTIFICATIONS);
        self.executor.start(Advertisement {
            service_uuid,
            payload,
            outbound: Arc::new(Mutex::new(Some(receiver))),
        })?;
        self.outbound = Some(sender);
        Ok(())
    }

    /// Queue a value for the centrals. `Full` asks the host to drain events
    /// and retry; it never waits, because the operation that would make room
    /// may itself be waiting for the host to drain.
    pub fn notify(&mut self, notification: Notification) -> Result<(), DeliveryError> {
        if notification.data.len() > MAX_VALUE || notification.uuid.len() > MAX_SERVICE_UUID {
            return Err(DeliveryError::PayloadTooLarge);
        }
        let sender = self.outbound.as_ref().ok_or(DeliveryError::Inactive)?;
        sender.try_send(notification).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => DeliveryError::Full,
            mpsc::error::TrySendError::Closed(_) => DeliveryError::Inactive,
        })
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.executor.pop()
    }

    pub fn stop(&mut self) {
        self.outbound = None;
        self.executor.stop();
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.executor.next_event().await
    }

    /// Close delivery, withdraw the advertisement and wait for its future to
    /// be dropped.
    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.outbound = None;
        self.executor.shutdown().await
    }
}
