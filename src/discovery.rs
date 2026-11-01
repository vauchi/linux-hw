// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::executor::Executor;
use crate::ports::{Discovery, ExecutorError};
use std::sync::Arc;
use tokio::runtime::Handle;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, InboxLimits};

/// Host-owned discovery executor. The host keeps its runtime alive through
/// `shutdown`, and serializes event dispatch with `start`, `stop` and disposal.
pub struct BleScanner {
    executor: Executor<String>,
}

impl BleScanner {
    pub fn with_backend(
        backend: Arc<dyn Discovery>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let native = move |service_uuid, events| {
            let backend = backend.clone();
            Box::pin(async move { backend.scan(service_uuid, events).await }) as _
        };
        Ok(Self {
            executor: Executor::new(native, &runtime, limits)?,
        })
    }

    /// Replace the native discovery operation. Core chooses its lifetime;
    /// this adapter imposes no scan duration or fallback behavior.
    pub fn start(&mut self, service_uuid: String) -> Result<(), DeliveryError> {
        self.executor.ensure_open()?;
        if service_uuid.len() > 128 {
            return Err(DeliveryError::PayloadTooLarge);
        }
        self.executor.start(service_uuid)
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.executor.pop()
    }

    pub fn stop(&mut self) {
        self.executor.stop();
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.executor.next_event().await
    }

    /// Close delivery, cancel pending OS work and wait for its future to be
    /// dropped. Native device acceptance must additionally verify OS cleanup.
    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.executor.shutdown().await
    }
}
