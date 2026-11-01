// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::delivery::EventSender;
use crate::ports::ExecutorError;
use crate::worker::NativeWork;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use vauchi_contract::Event;
use vauchi_contract::executor::{DeliveryError, EventInbox, InboxLimits, Operation};

/// One native operation at a time, of the kind `R` describes, with its events
/// delivered through a bounded inbox the host drains.
pub(crate) struct Executor<R> {
    inbox: EventInbox,
    capacity: Arc<Notify>,
    available: Arc<Notify>,
    requests: watch::Sender<Option<(R, Operation)>>,
    worker: Option<JoinHandle<()>>,
    active: Option<Operation>,
    closed: bool,
}

impl<R: Clone + Send + Sync + 'static> Executor<R> {
    pub(crate) fn new(
        native: impl Fn(R, EventSender) -> NativeWork + Send + Sync + 'static,
        runtime: &Handle,
        limits: InboxLimits,
    ) -> Result<Self, ExecutorError> {
        if limits.event_bytes.get() < 256 {
            return Err(ExecutorError::EventBudgetTooSmall);
        }
        let capacity = Arc::new(Notify::new());
        let available = Arc::new(Notify::new());
        let (requests, receiver) = watch::channel(None);
        let worker = runtime.spawn(crate::worker::run(
            native,
            receiver,
            capacity.clone(),
            available.clone(),
        ));
        Ok(Self {
            inbox: EventInbox::new(limits),
            capacity,
            available,
            requests,
            worker: Some(worker),
            active: None,
            closed: false,
        })
    }

    pub(crate) fn ensure_open(&self) -> Result<(), DeliveryError> {
        if self.closed || self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(DeliveryError::Closed);
        }
        Ok(())
    }

    pub(crate) fn start(&mut self, request: R) -> Result<(), DeliveryError> {
        self.ensure_open()?;
        self.stop();
        let operation = self.inbox.start()?;
        self.requests
            .send_replace(Some((request, operation.clone())));
        self.active = Some(operation);
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<Event> {
        let event = self.inbox.pop();
        if event.is_some() {
            self.capacity.notify_one();
        }
        event
    }

    pub(crate) fn stop(&mut self) {
        if let Some(operation) = self.active.take() {
            operation.cancel();
        }
        self.requests.send_replace(None);
        self.capacity.notify_waiters();
    }

    pub(crate) async fn next_event(&mut self) -> Option<Event> {
        let available = self.available.clone();
        loop {
            let ready = available.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if self.closed {
                return None;
            }
            if let Some(event) = self.pop() {
                return Some(event);
            }
            ready.await;
        }
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        self.closed = true;
        self.inbox.close();
        self.stop();
        let result = if let Some(worker) = self.worker.as_mut() {
            worker.abort();
            match worker.await {
                Ok(()) => Ok(()),
                Err(error) if error.is_cancelled() => Ok(()),
                Err(_) => Err(ExecutorError::WorkerFailed),
            }
        } else {
            Ok(())
        };
        self.worker.take();
        result
    }
}

impl<R> Drop for Executor<R> {
    fn drop(&mut self) {
        self.inbox.close();
        if let Some(operation) = self.active.take() {
            operation.cancel();
        }
        self.requests.send_replace(None);
        self.capacity.notify_waiters();
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}
