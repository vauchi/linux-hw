// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use futures_util::StreamExt;
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    os::fd::OwnedFd,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, OwnedVariant, Variant};

const SERVICE: &str = "org.freedesktop.portal.Desktop";
const CAMERA: &str = "org.freedesktop.portal.Camera";
const REQUEST: &str = "org.freedesktop.portal.Request";
const PATH: &str = "/org/freedesktop/portal/desktop";
static REQUESTS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PortalError {
    #[error("camera portal unavailable")]
    Unavailable,
    #[error("no camera is available")]
    NoCamera,
    #[error("camera access denied")]
    Denied,
    #[error("camera request cancelled")]
    Cancelled,
    #[error("invalid camera portal response")]
    InvalidResponse,
    #[error("camera portal request failed")]
    Failed,
}

#[derive(Clone)]
pub struct CameraPortal {
    connection: Connection,
}

#[derive(Clone)]
pub struct CameraGrant {
    connection: Connection,
}

impl CameraPortal {
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }

    pub async fn session() -> Result<Self, PortalError> {
        Connection::session()
            .await
            .map(Self::new)
            .map_err(|_| PortalError::Unavailable)
    }

    /// Probing presence never requests permission or opens a camera.
    pub async fn is_camera_present(&self) -> Result<bool, PortalError> {
        let reply = self
            .connection
            .call_method(
                Some(SERVICE),
                PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(CAMERA, "IsCameraPresent"),
            )
            .await
            .map_err(probe_error)?;
        let value: OwnedVariant = reply
            .body()
            .deserialize()
            .map_err(|_| PortalError::InvalidResponse)?;
        bool::try_from(value).map_err(|_| PortalError::InvalidResponse)
    }

    pub async fn request(&self, cancelled: Arc<AtomicBool>) -> Result<CameraGrant, PortalError> {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PortalError::Cancelled);
        }
        let sender = self
            .connection
            .unique_name()
            .ok_or(PortalError::Failed)?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = format!(
            "vauchi_{}_{}",
            std::process::id(),
            REQUESTS.fetch_add(1, Ordering::Relaxed)
        );
        let path = format!("{PATH}/request/{sender}/{token}");
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SERVICE)
            .path(path.as_str())
            .interface(REQUEST)
            .member("Response")
            .build()
            .map_err(|_| PortalError::Failed)?;
        // A portal may respond before AccessCamera returns its request handle.
        let mut responses =
            MessageStream::for_match_rule(rule, &self.connection, NonZeroUsize::new(4))
                .await
                .map_err(|_| PortalError::Failed)?;
        let options = HashMap::from([("handle_token", Variant::from(token.as_str()))]);
        let access = self.connection.call_method(
            Some(SERVICE),
            PATH,
            Some(CAMERA),
            "AccessCamera",
            &options,
        );
        tokio::pin!(access);
        let mut check_cancel = tokio::time::interval(Duration::from_millis(25));
        let reply = loop {
            if cancelled.load(Ordering::SeqCst) {
                self.close(&path).await;
                return Err(PortalError::Cancelled);
            }
            tokio::select! {
                biased;
                _ = check_cancel.tick() => {},
                reply = &mut access => break reply.map_err(|_| PortalError::Failed)?,
            }
        };
        let handle: OwnedObjectPath = reply
            .body()
            .deserialize()
            .map_err(|_| PortalError::InvalidResponse)?;
        if handle.as_str() != path {
            return Err(PortalError::InvalidResponse);
        }
        loop {
            if cancelled.load(Ordering::SeqCst) {
                self.close(&path).await;
                return Err(PortalError::Cancelled);
            }
            tokio::select! {
                biased;
                _ = check_cancel.tick() => {},
                reply = responses.next() => {
                    let reply = reply.ok_or(PortalError::Failed)?.map_err(|_| PortalError::Failed)?;
                    let (code, _): (u32, HashMap<String, OwnedVariant>) = reply.body().deserialize().map_err(|_| PortalError::InvalidResponse)?;
                    if cancelled.load(Ordering::SeqCst) {
                        self.close(&path).await;
                        return Err(PortalError::Cancelled);
                    }
                    return match code {
                        0 => Ok(CameraGrant { connection: self.connection.clone() }),
                        1 | 2 => Err(PortalError::Denied),
                        _ => Err(PortalError::InvalidResponse),
                    };
                }
            }
        }
    }

    async fn close(&self, path: &str) {
        let _ = tokio::time::timeout(
            Duration::from_millis(250),
            self.connection
                .call_method(Some(SERVICE), path, Some(REQUEST), "Close", &()),
        )
        .await;
    }
}

impl CameraGrant {
    pub async fn open_remote(&self) -> Result<OwnedFd, PortalError> {
        let options = HashMap::<&str, Variant<'_>>::new();
        let reply = self
            .connection
            .call_method(
                Some(SERVICE),
                PATH,
                Some(CAMERA),
                "OpenPipeWireRemote",
                &options,
            )
            .await
            .map_err(|_| PortalError::Denied)?;
        let fd: zbus::OwnedFd = reply
            .body()
            .deserialize()
            .map_err(|_| PortalError::InvalidResponse)?;
        Ok(fd.into())
    }
}

fn probe_error(error: zbus::Error) -> PortalError {
    let name = match error {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        zbus::Error::FDO(error) => zbus::DBusError::name(error.as_ref()).to_string(),
        _ => return PortalError::Failed,
    };
    match name.as_str() {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.UnknownInterface"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        | "org.freedesktop.DBus.Error.UnknownProperty" => PortalError::Unavailable,
        _ => PortalError::Failed,
    }
}
