// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::delivery::EventSender;
use crate::ports::HardwareFailure;
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Notify, watch};
use vauchi_contract::executor::Operation;

pub(crate) type NativeWork = Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send>>;

/// Runs one native operation per request, replacing it when the request
/// changes and dropping it when the request is withdrawn.
pub(crate) async fn run<R, F>(
    native: F,
    mut requests: watch::Receiver<Option<(R, Operation)>>,
    capacity: Arc<Notify>,
    available: Arc<Notify>,
) where
    R: Clone,
    F: Fn(R, EventSender) -> NativeWork,
{
    loop {
        let request = requests.borrow_and_update().clone();
        if let Some((request, operation)) = request {
            tokio::select! {
                biased;
                changed = requests.changed() => {
                    if changed.is_err() { return; }
                }
                () = execute(&native, request, operation, capacity.clone(), available.clone()) => {
                    if requests.changed().await.is_err() { return; }
                }
            }
        } else if requests.changed().await.is_err() {
            return;
        }
    }
}

async fn execute<R, F>(
    native: &F,
    request: R,
    operation: Operation,
    capacity: Arc<Notify>,
    available: Arc<Notify>,
) where
    F: Fn(R, EventSender) -> NativeWork,
{
    let events = EventSender {
        operation: operation.clone(),
        capacity: capacity.clone(),
        available: available.clone(),
    };
    let outcome = AssertUnwindSafe(async { native(request, events).await })
        .catch_unwind()
        .await
        .unwrap_or(Err(HardwareFailure::Failed));
    if let Err(failure) = outcome {
        let mut events = EventSender {
            operation: operation.clone(),
            capacity,
            available,
        };
        // The constructor reserves enough payload space for these static
        // failure events. Capacity is awaited, never silently discarded.
        let _ = events.send(failure.event()).await;
    }
    let _ = operation.finish();
}
