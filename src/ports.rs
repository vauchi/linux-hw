// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::delivery::EventSender;
use std::future::Future;
use std::pin::Pin;
use vauchi_contract::Event;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HardwareFailure {
    Unavailable,
    PermissionDenied,
    Failed,
    AdvertisingFailed,
    InvalidObservation,
}

impl HardwareFailure {
    pub(crate) fn event(self) -> Event {
        match self {
            Self::Unavailable => Event::HardwareUnavailable {
                transport: "BLE".into(),
            },
            Self::PermissionDenied => Event::PermissionDenied {
                transport: "BLE".into(),
            },
            Self::Failed | Self::AdvertisingFailed | Self::InvalidObservation => {
                Event::HardwareError {
                    transport: "BLE".into(),
                    error: match self {
                        Self::InvalidObservation => "Bluetooth observation exceeds executor limits",
                        Self::AdvertisingFailed => "Bluetooth advertising failed",
                        _ => "Bluetooth discovery failed",
                    }
                    .into(),
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecutorError {
    #[error("event budget must be at least 256 bytes to report hardware failures")]
    EventBudgetTooSmall,
    #[error("native discovery worker failed")]
    WorkerFailed,
}

/// A value Core writes on the inbound link (`BleWriteCharacteristic` with
/// `BleLinkDirection::Inbound`), sent to the centrals as a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub device_id: String,
    pub uuid: String,
    pub data: Vec<u8>,
}

/// The notifications Core sends while one advertisement runs; `None` once
/// the host stopped it.
pub struct Outbound(pub(crate) tokio::sync::mpsc::Receiver<Notification>);

impl Outbound {
    pub async fn next(&mut self) -> Option<Notification> {
        self.0.recv().await
    }
}

/// OS peripheral port: advertise, serve the exchange's GATT service, and
/// report what centrals do with it. Dropping the returned future withdraws
/// both the advertisement and the service.
pub trait Advertising: Send + Sync + 'static {
    fn advertise(
        &self,
        service_uuid: String,
        payload: Vec<u8>,
        outbound: Outbound,
        events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>>;
}

/// OS discovery port. Implementations bound capture buffers before constructing
/// events, and release native discovery when the returned future is dropped.
pub trait Discovery: Send + Sync + 'static {
    fn scan(
        &self,
        service_uuid: String,
        events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>>;
}
