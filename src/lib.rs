// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![forbid(unsafe_code)]

mod advertising;
mod audio;
#[cfg(target_os = "linux")]
mod bluez;
#[cfg(feature = "audio")]
mod cpal_audio;
mod delivery;
mod discovery;
mod executor;
#[cfg(all(target_os = "linux", feature = "fake-bluez"))]
pub mod fake_bluez;
mod host;
mod nfc;
#[cfg(feature = "nfc")]
mod pcsc;
mod ports;
/// The QR decoder (ADR-077: decoding is hardware-shell code), behind `qr-decode`.
#[cfg(feature = "qr-decode")]
pub mod qr;
mod worker;

pub use advertising::BleAdvertiser;
pub use audio::AudioSession;
#[cfg(target_os = "linux")]
pub use bluez::{BlueZAdvertising, BlueZDiscovery};
#[cfg(feature = "audio")]
pub use cpal_audio::CpalAudio;
pub use delivery::EventSender;
pub use discovery::BleScanner;
pub use host::{UnsupportedDiscovery, host_discovery};
pub use nfc::{NfcSession, ReaderSession};
#[cfg(feature = "nfc")]
pub use pcsc::PcscReader;
pub use ports::{
    Advertising, AudioBackend, Discovery, ExecutorError, HardwareFailure, NfcCard, NfcReader,
    Notification, Outbound,
};
