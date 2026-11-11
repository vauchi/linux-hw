// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::advertising::BleAdvertiser;
use crate::audio::AudioSession;
#[cfg(feature = "qr-decode")]
use crate::camera::CameraSession;
use crate::central::BleCentral;
use crate::direct::DirectSession;
use crate::discovery::BleScanner;
use crate::nfc::NfcSession;
use crate::notifications::NotificationSession;
use crate::ports::{ExecutorError, LinkRequest, Notification};
use crate::wakeup::WakeupTimer;
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

const SOURCES: usize = 9;

/// The entry point an app hands Core's hardware commands to (ADR-077
/// item 4, ADR-078 item 3). Each command goes to the session that owns
/// its port; the shell decides nothing beyond that.
#[derive(Default)]
pub struct HardwareShell {
    scanner: Option<BleScanner>,
    advertiser: Option<BleAdvertiser>,
    central: Option<BleCentral>,
    nfc: Option<NfcSession>,
    audio: Option<AudioSession>,
    #[cfg(feature = "qr-decode")]
    camera: Option<CameraSession>,
    direct: Option<DirectSession>,
    wakeup: WakeupTimer,
    notifications: Option<NotificationSession>,
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

    pub fn with_central(mut self, central: BleCentral) -> Self {
        self.central = Some(central);
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

    pub fn with_notifications(mut self, notifications: NotificationSession) -> Self {
        self.notifications = Some(notifications);
        self
    }

    pub fn with_direct(mut self, direct: DirectSession) -> Self {
        self.direct = Some(direct);
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
            Command::BleConnect { device_id } => {
                Ok(present(&mut self.central)?.connect(device_id)?)
            }
            Command::BleWriteCharacteristic {
                device_id,
                direction: BleLinkDirection::Outbound,
                uuid,
                data,
            } => Ok(present(&mut self.central)?
                .request(&device_id, LinkRequest::Write { uuid, data })?),
            Command::BleReadCharacteristic {
                device_id,
                direction: BleLinkDirection::Outbound,
                uuid,
            } => Ok(present(&mut self.central)?.request(&device_id, LinkRequest::Read { uuid })?),
            Command::BleDisconnect {
                device_id,
                direction: BleLinkDirection::Outbound,
            } => Ok(present(&mut self.central)?.disconnect(&device_id)?),
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
            Command::DirectSend {
                payload,
                is_initiator,
                endpoint,
            } => Ok(present(&mut self.direct)?.send(payload, is_initiator, endpoint)?),
            Command::PostNotification { notification } => {
                present(&mut self.notifications)?.post(notification);
                Ok(())
            }
            Command::ScheduleWakeup { delay_millis, .. } => {
                self.wakeup.schedule(delay_millis);
                Ok(())
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

    /// Wait for the next observation from any session; `None` once no
    /// session can report any more. Each session's wait gives up nothing
    /// when another wins the race, so none of its events is lost.
    pub async fn next_event(&mut self) -> Option<Event> {
        if let Some(event) = self.pop() {
            return Some(event);
        }
        let Self {
            scanner,
            advertiser,
            central,
            nfc,
            audio,
            #[cfg(feature = "qr-decode")]
            camera,
            direct,
            wakeup,
            notifications,
            ..
        } = self;
        #[cfg(feature = "qr-decode")]
        let camera = async {
            match camera {
                Some(camera) => camera.next_event().await,
                None => None,
            }
        };
        #[cfg(not(feature = "qr-decode"))]
        let camera = async { None::<Event> };
        tokio::select! {
            Some(event) = async { scanner.as_mut()?.next_event().await } => Some(event),
            Some(event) = async { advertiser.as_mut()?.next_event().await } => Some(event),
            Some(event) = async { central.as_mut()?.next_event().await } => Some(event),
            Some(event) = async { nfc.as_mut()?.next_event().await } => Some(event),
            Some(event) = async { audio.as_mut()?.next_event().await } => Some(event),
            Some(event) = camera => Some(event),
            Some(event) = async { direct.as_mut()?.next_event().await } => Some(event),
            Some(event) = wakeup.next_event() => Some(event),
            Some(event) = async { notifications.as_mut()?.next_event().await } => Some(event),
            else => None,
        }
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
        if let Some(central) = &mut self.central {
            central.shutdown().await?;
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
        if let Some(notifications) = &mut self.notifications {
            notifications.shutdown();
        }
        if let Some(direct) = &mut self.direct {
            direct.shutdown().await?;
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
            5 => self.central.as_mut().and_then(BleCentral::pop),
            6 => self.direct.as_mut().and_then(DirectSession::pop),
            7 => self.wakeup.pop(),
            8 => self
                .notifications
                .as_mut()
                .and_then(NotificationSession::pop),
            _ => None,
        }
    }
}

fn present<T>(session: &mut Option<T>) -> Result<&mut T, ExecuteError> {
    session.as_mut().ok_or(ExecuteError::Unsupported)
}
