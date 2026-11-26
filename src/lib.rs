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
#[cfg(feature = "qr-decode")]
pub mod camera_frame;
#[cfg(all(feature = "camera-preview", target_os = "linux"))]
pub mod camera_portal;
mod capabilities;
mod central;
mod delivery;
#[cfg(target_os = "linux")]
mod desktop_notifications;
mod direct;
mod discovery;
mod executor;
#[cfg(all(target_os = "linux", feature = "fake-bluez"))]
pub mod fake_bluez;
#[cfg(all(target_os = "linux", feature = "qr-decode"))]
pub mod frame_buffer;
mod host;
mod linux_hardware;
mod nfc;
#[cfg(feature = "camera")]
mod nokhwa_camera;
mod notifications;
#[cfg(feature = "nfc")]
mod pcsc;
#[cfg(all(feature = "camera-preview", target_os = "linux"))]
mod pipewire_camera;
mod ports;
#[cfg(all(feature = "camera-preview", target_os = "linux"))]
pub mod preview_source;
/// The QR decoder (ADR-077: decoding is hardware-shell code), behind `qr-decode`.
#[cfg(feature = "qr-decode")]
pub mod qr;
mod shell;
mod tcp_direct;
mod wakeup;
mod worker;

pub use advertising::BleAdvertiser;
#[cfg(feature = "audio")]
pub use alsa_audio::AlsaAudio;
pub use audio::AudioSession;
#[cfg(target_os = "linux")]
pub use bluez::{BlueZAdvertising, BlueZCentral, BlueZDiscovery};
#[cfg(feature = "qr-decode")]
pub use camera::{CameraSession, rgb_to_luma};
pub use capabilities::HostDevices;
pub use central::BleCentral;
pub use delivery::EventSender;
#[cfg(target_os = "linux")]
pub use desktop_notifications::DesktopNotifications;
pub use direct::DirectSession;
pub use discovery::BleScanner;
pub use host::{UnsupportedDiscovery, host_discovery};
pub use linux_hardware::{Backends, LinuxHardware, StartError};
pub use nfc::{NfcSession, ReaderSession};
#[cfg(feature = "camera")]
pub use nokhwa_camera::NokhwaCamera;
pub use notifications::NotificationSession;
#[cfg(feature = "nfc")]
pub use pcsc::PcscReader;
#[cfg(all(feature = "camera-preview", target_os = "linux"))]
pub use pipewire_camera::PipeWireCamera;
pub use ports::{
    Advertising, AudioBackend, CameraBackend, CameraFrames, Central, DirectBackend, Discovery,
    ExecutorError, HardwareFailure, LinkRequest, LinkRequests, LumaFrame, NfcCard, NfcReader,
    Notification, Notifier, Outbound,
};
pub use shell::{ExecuteError, HardwareShell};
pub use tcp_direct::TcpDirect;
