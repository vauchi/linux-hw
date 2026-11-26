// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;
use vauchi_contract::Event;
use vauchi_contract::exchange_types::{AudioCapability, DeviceCapabilities, Platform};

/// The devices present on the host, before asking whether linux-hw can
/// drive them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostDevices {
    pub bluetooth_adapter: bool,
    pub sound_card: bool,
    pub video_device: bool,
    pub nfc_reader: bool,
}

impl HostDevices {
    /// The running host's devices, the NFC reader through PC/SC.
    pub fn probe() -> Self {
        let devices = Self {
            nfc_reader: pcsc_reader_present(),
            ..Self::under(Path::new("/"))
        };
        #[cfg(all(feature = "camera-preview", target_os = "linux"))]
        let devices = Self {
            video_device: crate::PipeWireCamera::available(),
            ..devices
        };
        devices
    }

    /// The devices a host tree rooted at `root` names in sysfs, procfs and
    /// `/dev`. PC/SC readers are not files, so `nfc_reader` stays false.
    pub fn under(root: &Path) -> Self {
        Self {
            bluetooth_adapter: has_entries(&root.join("sys/class/bluetooth"), |_| true),
            sound_card: std::fs::read_to_string(root.join("proc/asound/cards"))
                .is_ok_and(|cards| cards.lines().any(names_a_sound_card)),
            video_device: has_entries(&root.join("dev"), |name| name.starts_with("video")),
            nfc_reader: false,
        }
    }

    /// What linux-hw can do here (ADR-077 item 7). Core replaces its device
    /// capabilities with this report, so every field is filled: a desktop
    /// with a network, as the terminals already report.
    pub fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            has_ble: cfg!(target_os = "linux") && self.bluetooth_adapter,
            has_nfc: cfg!(feature = "nfc") && self.nfc_reader,
            has_camera: cfg!(any(feature = "camera", feature = "camera-preview"))
                && self.video_device,
            audio: if cfg!(feature = "audio") && self.sound_card {
                AudioCapability::Full
            } else {
                AudioCapability::None
            },
            has_internet: true,
            platform: Platform::Desktop,
            ..DeviceCapabilities::default()
        }
    }

    pub fn report(&self) -> Event {
        Event::HardwareCapabilitiesReported {
            capabilities: self.capabilities(),
        }
    }
}

/// ALSA writes `--- no soundcards ---` when it has none.
fn names_a_sound_card(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.contains("no soundcards")
}

fn has_entries(dir: &Path, matches: impl Fn(&str) -> bool) -> bool {
    dir.read_dir().is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_str().is_some_and(&matches))
    })
}

#[cfg(feature = "nfc")]
fn pcsc_reader_present() -> bool {
    pcsc::Context::establish(pcsc::Scope::System)
        .and_then(|context| {
            let mut names = vec![0u8; context.list_readers_len()?];
            Ok(context.list_readers(&mut names)?.next().is_some())
        })
        .unwrap_or(false)
}

#[cfg(not(feature = "nfc"))]
fn pcsc_reader_present() -> bool {
    false
}
