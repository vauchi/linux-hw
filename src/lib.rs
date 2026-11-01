// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![forbid(unsafe_code)]

mod advertising;
#[cfg(target_os = "linux")]
mod bluez;
mod delivery;
mod discovery;
mod executor;
#[cfg(all(target_os = "linux", feature = "fake-bluez"))]
pub mod fake_bluez;
mod host;
mod ports;
mod worker;

pub use advertising::BleAdvertiser;
#[cfg(target_os = "linux")]
pub use bluez::{BlueZAdvertising, BlueZDiscovery};
pub use delivery::EventSender;
pub use discovery::BleScanner;
pub use host::{UnsupportedDiscovery, host_discovery};
pub use ports::{Advertising, Discovery, ExecutorError, HardwareFailure, Notification, Outbound};
