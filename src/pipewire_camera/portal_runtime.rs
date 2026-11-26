// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    camera_portal::{CameraPortal, PortalError},
    preview_source::RemoteOpener,
};
use std::{
    future::Future,
    os::fd::OwnedFd,
    sync::{Arc, LazyLock, atomic::AtomicBool},
    time::Duration,
};

static RUNTIME: LazyLock<Result<tokio::runtime::Runtime, std::io::Error>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("vauchi-camera-portal")
        .enable_all()
        .build()
});

fn run<T: Send + 'static>(
    future: impl Future<Output = Result<T, PortalError>> + Send + 'static,
) -> Result<T, PortalError> {
    let runtime = RUNTIME.as_ref().map_err(|_| PortalError::Failed)?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    runtime.spawn(async move {
        let _ = sender.send(future.await);
    });
    receiver.recv().map_err(|_| PortalError::Failed)?
}

pub(super) fn portal(
    supplied: Option<CameraPortal>,
    required: bool,
) -> Result<Option<CameraPortal>, PortalError> {
    if supplied.is_none() && !required {
        return Ok(None);
    }
    run(async move {
        let portal = match supplied {
            Some(portal) => Ok(portal),
            None => CameraPortal::session().await,
        };
        let result = match portal {
            Ok(portal) => {
                match tokio::time::timeout(Duration::from_secs(2), portal.is_camera_present()).await
                {
                    Ok(Ok(true)) => return Ok(Some(portal)),
                    Ok(Ok(false)) => return Err(PortalError::NoCamera),
                    Ok(Err(error)) => error,
                    Err(_) => PortalError::Failed,
                }
            }
            Err(error) => error,
        };
        if result == PortalError::Unavailable && !required {
            Ok(None)
        } else {
            Err(result)
        }
    })
}

pub(super) struct Access {
    pub fd: Option<OwnedFd>,
    pub remote: Option<RemoteOpener>,
}

pub(super) fn authorize(
    supplied: Option<CameraPortal>,
    required: bool,
    cancelled: Arc<AtomicBool>,
) -> Result<Access, PortalError> {
    let Some(portal) = portal(supplied, required)? else {
        return Ok(Access {
            fd: None,
            remote: None,
        });
    };
    run(async move {
        let grant = portal.request(cancelled).await?;
        let fd = tokio::time::timeout(Duration::from_millis(500), grant.open_remote())
            .await
            .map_err(|_| PortalError::Failed)??;
        let remote: RemoteOpener = Arc::new(move || {
            let grant = grant.clone();
            run(async move {
                tokio::time::timeout(Duration::from_millis(500), grant.open_remote())
                    .await
                    .map_err(|_| PortalError::Failed)?
            })
            .map_err(|error| error.to_string())
        });
        Ok(Access {
            fd: Some(fd),
            remote: Some(remote),
        })
    })
}

pub(super) fn sandboxed() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists()
}
