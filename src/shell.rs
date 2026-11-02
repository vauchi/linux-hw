// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::advertising::BleAdvertiser;
use crate::audio::AudioSession;
#[cfg(feature = "qr-decode")]
use crate::camera::CameraSession;
use crate::discovery::BleScanner;
use crate::nfc::NfcSession;
use crate::ports::{ExecutorError, Notification};
use vauchi_contract::executor::DeliveryError;
use vauchi_contract::{BleLinkDirection, Command, Event};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecuteError {
    /// linux-hw has no port for this command on this host. The shell
    /// reports nothing: its capability report tells Core what it runs
    /// (owner decision 2026-11-02, private#770).
    #[error("this hardware shell cannot run the command")]
    Unsupported,
    #[error(transparent)]
    Delivery(#[from] DeliveryError),
}

const SOURCES: usize = 5;

/// The entry point an app hands Core's hardware commands to (ADR-077
/// item 4, ADR-078 item 3). Each command goes to the session that owns
/// its port; the shell decides nothing beyond that.
#[derive(Default)]
pub struct HardwareShell {
    scanner: Option<BleScanner>,
    advertiser: Option<BleAdvertiser>,
    nfc: Option<NfcSession>,
    audio: Option<AudioSession>,
    #[cfg(feature = "qr-decode")]
    camera: Option<CameraSession>,
    next_source: usize,
}

impl HardwareShell {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_scanner(mut self, scanner: BleScanner) -> Self {
        self.scanner = Some(scanner);
        self
    }

    pub fn with_advertiser(mut self, advertiser: BleAdvertiser) -> Self {
        self.advertiser = Some(advertiser);
        self
    }

    pub fn with_nfc(mut self, nfc: NfcSession) -> Self {
        self.nfc = Some(nfc);
        self
    }

    pub fn with_audio(mut self, audio: AudioSession) -> Self {
        self.audio = Some(audio);
        self
    }

    #[cfg(feature = "qr-decode")]
    pub fn with_camera(mut self, camera: CameraSession) -> Self {
        self.camera = Some(camera);
        self
    }

    pub fn execute(&mut self, command: Command) -> Result<(), ExecuteError> {
        match command {
            Command::BleStartScanning { service_uuid } => {
                Ok(present(&mut self.scanner)?.start(service_uuid)?)
            }
            Command::BleStopScanning => present(&mut self.scanner).map(BleScanner::stop),
            Command::BleStartAdvertising {
                service_uuid,
                payload,
            } => Ok(present(&mut self.advertiser)?.start(service_uuid, payload)?),
            Command::BleStopAdvertising => present(&mut self.advertiser).map(BleAdvertiser::stop),
            Command::BleWriteCharacteristic {
                device_id,
                direction: BleLinkDirection::Inbound,
                uuid,
                data,
            } => Ok(present(&mut self.advertiser)?.notify(Notification {
                device_id,
                uuid,
                data,
            })?),
            Command::NfcActivate { apdus, .. } => Ok(present(&mut self.nfc)?.activate(apdus)?),
            Command::NfcSendApdu { apdus, .. } => Ok(present(&mut self.nfc)?.send(apdus)?),
            Command::NfcDeactivate => present(&mut self.nfc).map(NfcSession::deactivate),
            Command::AudioEmitChallenge {
                samples,
                sample_rate,
            } => Ok(present(&mut self.audio)?.emit_challenge(samples, sample_rate)?),
            Command::AudioListenForResponse { timeout_ms, .. } => {
                Ok(present(&mut self.audio)?.listen_for_response(timeout_ms)?)
            }
            Command::AudioStop => present(&mut self.audio).map(AudioSession::stop),
            #[cfg(feature = "qr-decode")]
            Command::CameraCapture {
                preview_id,
                purpose,
            } => Ok(present(&mut self.camera)?.capture(preview_id, purpose)?),
            #[cfg(feature = "qr-decode")]
            Command::CameraStop { preview_id } => {
                present(&mut self.camera).map(|camera| camera.stop(&preview_id))
            }
            _ => Err(ExecuteError::Unsupported),
        }
    }

    /// The next observation from any session. Starts one session further
    /// on each call so a busy one cannot starve the others.
    pub fn pop(&mut self) -> Option<Event> {
        let first = self.next_source;
        self.next_source = (first + 1) % SOURCES;
        (0..SOURCES).find_map(|offset| self.pop_source((first + offset) % SOURCES))
    }

    /// Withdraw every running operation and wait until each session has
    /// dropped its native work.
    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        if let Some(scanner) = &mut self.scanner {
            scanner.shutdown().await?;
        }
        if let Some(advertiser) = &mut self.advertiser {
            advertiser.shutdown().await?;
        }
        if let Some(nfc) = &mut self.nfc {
            nfc.shutdown().await?;
        }
        if let Some(audio) = &mut self.audio {
            audio.shutdown().await?;
        }
        #[cfg(feature = "qr-decode")]
        if let Some(camera) = &mut self.camera {
            camera.shutdown().await?;
        }
        Ok(())
    }

    fn pop_source(&mut self, source: usize) -> Option<Event> {
        match source {
            0 => self.scanner.as_mut().and_then(BleScanner::pop),
            1 => self.advertiser.as_mut().and_then(BleAdvertiser::pop),
            2 => self.nfc.as_mut().and_then(NfcSession::pop),
            3 => self.audio.as_mut().and_then(AudioSession::pop),
            #[cfg(feature = "qr-decode")]
            4 => self.camera.as_mut().and_then(CameraSession::pop),
            _ => None,
        }
    }
}

fn present<T>(session: &mut Option<T>) -> Result<&mut T, ExecuteError> {
    session.as_mut().ok_or(ExecuteError::Unsupported)
}
