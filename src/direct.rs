// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contract's direct-send executor (private#1029 4d.7c).
//!
//! `DirectSend` runs one exchange with the peer at Core's endpoint and
//! reports the peer's payload as `DirectPayloadReceived`, or the failure as
//! a `HardwareError` on "USB", the transport the display shells always
//! named. The backend blocks, so it runs on a runtime blocking thread.
use crate::delivery::EventSender;
use crate::executor::Executor;
use crate::ports::{DirectBackend, ExecutorError, HardwareFailure};
use std::sync::Arc;
use tokio::runtime::Handle;
use vauchi_contract::executor::{DeliveryError, InboxLimits};
use vauchi_contract::{DirectEndpoint, Event};

/// The frame codec's bound (contract `direct`, 64 KiB), checked before the
/// payload reaches a thread.
const MAX_PAYLOAD: usize = 65_536;
const TRANSPORT: &str = "USB";

#[derive(Clone)]
struct Exchange {
    payload: Arc<Vec<u8>>,
    is_initiator: bool,
    endpoint: Arc<DirectEndpoint>,
}

pub struct DirectSession {
    exchanges: Executor<Exchange>,
}

impl DirectSession {
    pub fn with_backend(
        backend: Arc<dyn DirectBackend>,
        runtime: Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        let run = move |request: Exchange, events: EventSender| {
            let backend = backend.clone();
            Box::pin(async move {
                let outcome = tokio::task::spawn_blocking(move || {
                    backend.exchange(&request.endpoint, &request.payload, request.is_initiator)
                })
                .await
                .map_err(|_| HardwareFailure::Failed)?;
                let event = match outcome {
                    Ok(data) => Event::DirectPayloadReceived { data },
                    Err(error) => Event::HardwareError {
                        transport: TRANSPORT.into(),
                        error,
                    },
                };
                send(events, event).await
            }) as _
        };
        Ok(Self {
            exchanges: Executor::new(run, &runtime, limits)?,
        })
    }

    /// `DirectSend`: exchange `payload` with the peer, in the given role.
    pub fn send(
        &mut self,
        payload: Vec<u8>,
        is_initiator: bool,
        endpoint: DirectEndpoint,
    ) -> Result<(), DeliveryError> {
        self.exchanges.ensure_open()?;
        if payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return Err(DeliveryError::PayloadTooLarge);
        }
        self.exchanges.start(Exchange {
            payload: Arc::new(payload),
            is_initiator,
            endpoint: Arc::new(endpoint),
        })
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.exchanges.pop()
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.exchanges.next_event().await
    }

    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.exchanges.shutdown().await
    }
}

async fn send(mut events: EventSender, event: Event) -> Result<(), HardwareFailure> {
    events
        .send(event)
        .await
        .map_err(|_| HardwareFailure::Failed)
}
