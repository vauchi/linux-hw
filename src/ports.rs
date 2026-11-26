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
    LinkFailed,
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
            Self::Failed
            | Self::AdvertisingFailed
            | Self::LinkFailed
            | Self::InvalidObservation => Event::HardwareError {
                transport: "BLE".into(),
                error: match self {
                    Self::InvalidObservation => "Bluetooth observation exceeds executor limits",
                    Self::AdvertisingFailed => "Bluetooth advertising failed",
                    Self::LinkFailed => "Bluetooth link failed",
                    _ => "Bluetooth discovery failed",
                }
                .into(),
            },
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

/// What Core asks of the outbound link while it is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkRequest {
    Write { uuid: String, data: Vec<u8> },
    Read { uuid: String },
}

/// Core's requests for one outbound link; `None` once the host dropped it.
pub struct LinkRequests(pub(crate) tokio::sync::mpsc::Receiver<LinkRequest>);

impl LinkRequests {
    pub async fn next(&mut self) -> Option<LinkRequest> {
        self.0.recv().await
    }
}

/// OS central port: dial the peer's GATT server and carry Core's requests
/// on that link. The backend reports `BleConnected`, what the peer sends,
/// read answers and `BleDisconnected`, all with `BleLinkDirection::Outbound`.
/// Dropping the returned future disconnects.
pub trait Central: Send + Sync + 'static {
    fn connect(
        &self,
        device_id: String,
        requests: LinkRequests,
        events: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<(), HardwareFailure>> + Send + '_>>;
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

/// A connected NFC tag: transmits one APDU and returns the raw answer,
/// status word included. The hardware shell interprets nothing.
pub trait NfcCard: Send {
    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, String>;
}

/// The NFC reader port: blocks until a tag connects or `cancelled` is
/// set (PC/SC offers no arrival callback, so the wait polls), and names
/// why it could not connect.
pub trait NfcReader: Send + Sync + 'static {
    fn wait_for_tag(
        &self,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn NfcCard>, String>;
}

/// The audio port: plays Core's pre-encoded samples and records for a
/// while, both blocking, both ending early when `cancelled` is set. The
/// modulation lives in Core; this is a pure hardware adapter.
pub trait AudioBackend: Send + Sync + 'static {
    fn emit(
        &self,
        samples: &[f32],
        sample_rate: u32,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String>;

    fn record(
        &self,
        timeout: std::time::Duration,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<(Vec<f32>, u32), String>;
}

/// One camera frame as the 8-bit Y plane the decoder reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LumaFrame {
    pub luma: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// An open camera stream: blocks for the next frame, `None` once the
/// stream ends. A live camera ends only when `cancelled` is set. Opened
/// and read on one blocking thread, so it need not be `Send` (V4L2
/// handles are not).
pub trait CameraFrames {
    fn next_luma(&mut self) -> Result<Option<LumaFrame>, String>;
}

/// The camera port: opens the device's stream, or names why it cannot.
pub trait CameraBackend: Send + Sync + 'static {
    #[cfg(all(feature = "camera-preview", target_os = "linux"))]
    fn preview_directory(&self) -> Option<crate::preview_source::PreviewDirectory> {
        None
    }

    fn open(
        &self,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Box<dyn CameraFrames>, String>;

    /// Open capture and its matching hardware-owned preview stream.
    /// Capture-only ports retain their existing behavior.
    fn open_preview(
        &self,
        _preview_id: &str,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Box<dyn CameraFrames>, String> {
        self.open(cancelled)
    }
}

/// The direct-send port: one framed exchange with the peer at Core's
/// endpoint over a wired or local link, blocking until the peer's payload
/// arrives or Core's deadlines pass. Core chose the endpoint, role and
/// deadlines (ADR-077); this is a pure hardware adapter.
pub trait DirectBackend: Send + Sync + 'static {
    fn exchange(
        &self,
        endpoint: &vauchi_contract::DirectEndpoint,
        payload: &[u8],
        is_initiator: bool,
    ) -> Result<Vec<u8>, String>;
}

/// The desktop notification port (ADR-077). `post` shows Core's prepared
/// notification, replacing one with the same `notification_id`;
/// `activations` runs until dropped and passes on the deep link of each
/// notification the user clicks. The port interprets nothing.
pub trait Notifier: Send + Sync + 'static {
    fn post(
        &self,
        notification: vauchi_contract::NotificationSpec,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn activations(
        &self,
        opened: tokio::sync::mpsc::Sender<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}
