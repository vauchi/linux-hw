// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![forbid(unsafe_code)]

mod advertising;
#[cfg(feature = "audio")]
mod alsa_audio;
mod audio;
#[cfg(target_os = "linux")]
mod bluez;
#[cfg(feature = "qr-decode")]
mod camera;
mod capabilities;
mod central;
mod delivery;
mod discovery;
mod executor;
#[cfg(all(target_os = "linux", feature = "fake-bluez"))]
pub mod fake_bluez;
mod host;
mod linux_hardware;
mod nfc;
#[cfg(feature = "camera")]
mod nokhwa_camera;
#[cfg(feature = "nfc")]
mod pcsc;
mod ports;
/// The QR decoder (ADR-077: decoding is hardware-shell code), behind `qr-decode`.
#[cfg(feature = "qr-decode")]
pub mod qr;
mod shell;
mod worker;

pub use advertising::BleAdvertiser;
#[cfg(feature = "audio")]
pub use alsa_audio::AlsaAudio;
pub use audio::AudioSession;
#[cfg(target_os = "linux")]
pub use bluez::{BlueZAdvertising, BlueZDiscovery};
#[cfg(feature = "qr-decode")]
pub use camera::{CameraSession, rgb_to_luma};
pub use capabilities::HostDevices;
pub use central::BleCentral;
pub use delivery::EventSender;
pub use discovery::BleScanner;
pub use host::{UnsupportedDiscovery, host_discovery};
pub use linux_hardware::{Backends, LinuxHardware, StartError};
pub use nfc::{NfcSession, ReaderSession};
#[cfg(feature = "camera")]
pub use nokhwa_camera::NokhwaCamera;
#[cfg(feature = "nfc")]
pub use pcsc::PcscReader;
pub use ports::{
    Advertising, AudioBackend, CameraBackend, CameraFrames, Central, Discovery, ExecutorError,
    HardwareFailure, LinkRequest, LinkRequests, LumaFrame, NfcCard, NfcReader, Notification,
    Outbound,
};
pub use shell::{ExecuteError, HardwareShell};
