// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `HardwareShell` hands Core's NFC, audio and camera commands to the
//! session that owns each port; their events come back through `pop`.
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use vauchi_contract::executor::InboxLimits;
use vauchi_contract::{Command, Event};
use vauchi_linux_hw::{
    AudioBackend, AudioSession, ExecuteError, HardwareShell, NfcCard, NfcReader, NfcSession,
};

fn limits() -> InboxLimits {
    InboxLimits {
        operations: NonZeroUsize::new(1).unwrap(),
        events: NonZeroUsize::new(4).unwrap(),
        event_bytes: NonZeroUsize::new(4096).unwrap(),
    }
}

async fn next_event(shell: &mut HardwareShell) -> Event {
    loop {
        if let Some(event) = shell.pop() {
            return event;
        }
        tokio::task::yield_now().await;
    }
}

struct EchoCard;

impl NfcCard for EchoCard {
    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, String> {
        Ok([apdu, &[0x90, 0x00]].concat())
    }
}

struct TagPresent;

impl NfcReader for TagPresent {
    fn wait_for_tag(&self, _: &AtomicBool) -> Result<Box<dyn NfcCard>, String> {
        Ok(Box::new(EchoCard))
    }
}

struct FixedAudio;

impl AudioBackend for FixedAudio {
    fn emit(&self, _: &[f32], _: u32, _: &AtomicBool) -> Result<(), String> {
        Ok(())
    }

    fn record(&self, _: Duration, _: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
        Ok((vec![0.25, -0.5], 44_100))
    }
}

// @scenario: contact_exchange.feature :: NFC active exchange between two phones
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_nfc_activation_transmits_cores_frames_and_the_answer_reaches_pop() {
    let runtime = tokio::runtime::Handle::current();
    let nfc = NfcSession::with_backend(Arc::new(TagPresent), runtime, limits()).unwrap();
    let mut shell = HardwareShell::new().with_nfc(nfc);

    shell
        .execute(Command::NfcActivate {
            payload: Vec::new(),
            apdus: vec![vec![0x00, 0xA4]],
        })
        .unwrap();

    assert_eq!(
        next_event(&mut shell).await,
        Event::NfcApduReceived {
            bytes: vec![0x00, 0xA4, 0x90, 0x00]
        }
    );
    shell.execute(Command::NfcDeactivate).unwrap();
    shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_audio_listen_reports_the_recorded_samples_through_pop() {
    let runtime = tokio::runtime::Handle::current();
    let audio = AudioSession::with_backend(Arc::new(FixedAudio), runtime, limits()).unwrap();
    let mut shell = HardwareShell::new().with_audio(audio);

    shell
        .execute(Command::AudioListenForResponse {
            timeout_ms: 10,
            sample_rate: 48_000,
        })
        .unwrap();

    assert_eq!(
        next_event(&mut shell).await,
        Event::AudioSamplesRecorded {
            samples: vec![0.25, -0.5],
            sample_rate: 44_100
        }
    );
    shell
        .execute(Command::AudioEmitChallenge {
            samples: vec![0.1],
            sample_rate: 48_000,
        })
        .unwrap();
    shell.execute(Command::AudioStop).unwrap();
    shell.shutdown().await.unwrap();
}

// @internal
#[tokio::test]
async fn nfc_and_audio_commands_are_unsupported_without_their_session() {
    let mut shell = HardwareShell::new();

    for command in [
        Command::NfcActivate {
            payload: Vec::new(),
            apdus: vec![vec![0x00]],
        },
        Command::NfcSendApdu {
            data: Vec::new(),
            apdus: vec![vec![0x00]],
        },
        Command::NfcDeactivate,
        Command::AudioEmitChallenge {
            samples: vec![0.1],
            sample_rate: 48_000,
        },
        Command::AudioStop,
    ] {
        let name = format!("{command:?}");
        assert_eq!(
            shell.execute(command),
            Err(ExecuteError::Unsupported),
            "{name}"
        );
    }
    assert_eq!(shell.pop(), None);
}

#[cfg(feature = "qr-decode")]
mod camera {
    use super::*;
    use vauchi_contract::CameraPurpose;
    use vauchi_linux_hw::{CameraBackend, CameraFrames, CameraSession};

    struct NoCamera;

    impl CameraBackend for NoCamera {
        fn open(&self, _: Arc<AtomicBool>) -> Result<Box<dyn CameraFrames>, String> {
            Err("no camera at /dev/video0".into())
        }
    }

    // @internal
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_camera_capture_runs_on_the_camera_session() {
        let runtime = tokio::runtime::Handle::current();
        let camera = CameraSession::with_backend(Arc::new(NoCamera), runtime, limits()).unwrap();
        let mut shell = HardwareShell::new().with_camera(camera);

        shell
            .execute(Command::CameraCapture {
                preview_id: "preview-1".into(),
                purpose: CameraPurpose::QrScan,
            })
            .unwrap();

        assert_eq!(
            next_event(&mut shell).await,
            Event::HardwareError {
                transport: "Camera".into(),
                error: "no camera at /dev/video0".into()
            }
        );
        shell
            .execute(Command::CameraStop {
                preview_id: "preview-1".into(),
            })
            .unwrap();
        shell.shutdown().await.unwrap();
    }

    // @internal
    #[tokio::test]
    async fn camera_commands_are_unsupported_without_a_camera_session() {
        let mut shell = HardwareShell::new();

        assert_eq!(
            shell.execute(Command::CameraStop {
                preview_id: "preview-1".into()
            }),
            Err(ExecuteError::Unsupported)
        );
    }
}
