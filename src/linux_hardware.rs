// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::advertising::BleAdvertiser;
use crate::audio::AudioSession;
#[cfg(feature = "qr-decode")]
use crate::camera::CameraSession;
use crate::capabilities::HostDevices;
use crate::central::BleCentral;
use crate::discovery::BleScanner;
use crate::nfc::NfcSession;
#[cfg(feature = "qr-decode")]
use crate::ports::CameraBackend;
use crate::ports::{Advertising, AudioBackend, Central, Discovery, ExecutorError, NfcReader};
use crate::shell::{ExecuteError, HardwareShell};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::runtime::{Handle, Runtime};
use vauchi_contract::executor::InboxLimits;
use vauchi_contract::{Command, Event};

/// One scan at a time; 64 events is the native queue chosen for BlueZ, and
/// 2048 bytes holds the largest observation Core accepts (private#770,
/// "Buffering proposal").
const DISCOVERY_LIMITS: InboxLimits = limits(64, 2048);
/// A 512-byte attribute value, ATT's longest, is about 2.1 KB as JSON.
const GATT_LIMITS: InboxLimits = limits(64, 4096);
/// A second of 48 kHz f32 samples is 192 KB.
const AUDIO_LIMITS: InboxLimits = limits(16, 4 << 20);
const NFC_LIMITS: InboxLimits = limits(16, 4096);
#[cfg(feature = "qr-decode")]
const CAMERA_LIMITS: InboxLimits = limits(16, 1 << 16);

const fn limits(events: usize, event_bytes: usize) -> InboxLimits {
    InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(events).unwrap(),
        event_bytes: NonZeroUsize::new(event_bytes).unwrap(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("hardware runtime: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("hardware session: {0}")]
    Session(#[from] ExecutorError),
}

/// The OS backends the sessions run on; a missing one leaves its commands
/// unsupported.
#[derive(Default)]
pub struct Backends {
    pub discovery: Option<Arc<dyn Discovery>>,
    pub advertising: Option<Arc<dyn Advertising>>,
    pub central: Option<Arc<dyn Central>>,
    pub nfc: Option<Arc<dyn NfcReader>>,
    pub audio: Option<Arc<dyn AudioBackend>>,
    #[cfg(feature = "qr-decode")]
    pub camera: Option<Arc<dyn CameraBackend>>,
}

impl Backends {
    /// The native backend for each device the report claims, so the shell
    /// runs exactly what it told Core it can.
    pub fn host(devices: &HostDevices) -> Self {
        let claimed = devices.capabilities();
        let mut backends = Self::default();
        #[cfg(target_os = "linux")]
        if claimed.has_ble {
            backends.discovery = Some(Arc::new(crate::bluez::BlueZDiscovery::system()));
            backends.advertising = Some(Arc::new(crate::bluez::BlueZAdvertising::system()));
        }
        #[cfg(feature = "nfc")]
        if claimed.has_nfc {
            backends.nfc = Some(Arc::new(crate::pcsc::PcscReader));
        }
        #[cfg(feature = "audio")]
        if claimed.audio != vauchi_contract::exchange_types::AudioCapability::None {
            backends.audio = Some(Arc::new(crate::alsa_audio::AlsaAudio));
        }
        #[cfg(feature = "camera")]
        if claimed.has_camera {
            backends.camera = Some(Arc::new(crate::nokhwa_camera::NokhwaCamera));
        }
        let _ = claimed;
        backends
    }
}

/// What an app holds: linux-hw's runtime, the sessions on it and the
/// capability report, behind blocking calls so the app needs no runtime.
pub struct LinuxHardware {
    shell: HardwareShell,
    report: Event,
    // Dropped after `shell`, whose shutdown runs on it.
    runtime: Runtime,
}

impl LinuxHardware {
    /// Probe the host and attach the native backend of every device found.
    pub fn start() -> Result<Self, StartError> {
        let devices = HostDevices::probe();
        Self::with_backends(Backends::host(&devices), devices)
    }

    pub fn with_backends(backends: Backends, devices: HostDevices) -> Result<Self, StartError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("vauchi-linux-hw")
            .enable_all()
            .build()?;
        let shell = attach(backends, runtime.handle())?;
        Ok(Self {
            shell,
            report: devices.report(),
            runtime,
        })
    }

    /// The `HardwareCapabilitiesReported` the app sends Core at start.
    pub fn report(&self) -> Event {
        self.report.clone()
    }

    pub fn execute(&mut self, command: Command) -> Result<(), ExecuteError> {
        self.shell.execute(command)
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.shell.pop()
    }
}

impl Drop for LinuxHardware {
    fn drop(&mut self) {
        let _ = self.runtime.block_on(self.shell.shutdown());
    }
}

fn attach(backends: Backends, runtime: &Handle) -> Result<HardwareShell, ExecutorError> {
    let mut shell = HardwareShell::new();
    if let Some(discovery) = backends.discovery {
        shell = shell.with_scanner(BleScanner::with_backend(
            discovery,
            runtime.clone(),
            DISCOVERY_LIMITS,
        )?);
    }
    if let Some(advertising) = backends.advertising {
        shell = shell.with_advertiser(BleAdvertiser::with_backend(
            advertising,
            runtime.clone(),
            GATT_LIMITS,
        )?);
    }
    if let Some(central) = backends.central {
        shell = shell.with_central(BleCentral::with_backend(
            central,
            runtime.clone(),
            GATT_LIMITS,
        )?);
    }
    if let Some(nfc) = backends.nfc {
        shell = shell.with_nfc(NfcSession::with_backend(nfc, runtime.clone(), NFC_LIMITS)?);
    }
    if let Some(audio) = backends.audio {
        shell = shell.with_audio(AudioSession::with_backend(
            audio,
            runtime.clone(),
            AUDIO_LIMITS,
        )?);
    }
    #[cfg(feature = "qr-decode")]
    if let Some(camera) = backends.camera {
        shell = shell.with_camera(CameraSession::with_backend(
            camera,
            runtime.clone(),
            CAMERA_LIMITS,
        )?);
    }
    Ok(shell)
}
