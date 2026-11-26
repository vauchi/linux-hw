// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The capability report (ADR-077 item 7): linux-hw names the devices on
//! the host, and claims one only when it was built with that device's port.
use std::fs;
use std::path::Path;
use vauchi_contract::Event;
use vauchi_contract::exchange_types::{AudioCapability, DeviceCapabilities, Platform};
use vauchi_linux_hw::HostDevices;

fn touch(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn every_device() -> HostDevices {
    HostDevices {
        bluetooth_adapter: true,
        sound_card: true,
        video_device: true,
        nfc_reader: true,
    }
}

// @internal
#[test]
fn the_host_tree_names_its_adapter_sound_card_and_camera() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("sys/class/bluetooth/hci0")).unwrap();
    touch(
        root.path(),
        "proc/asound/cards",
        " 0 [Device         ]: USB-Audio - USB Audio Device\n",
    );
    touch(root.path(), "dev/video0", "");

    assert_eq!(
        HostDevices::under(root.path()),
        HostDevices {
            bluetooth_adapter: true,
            sound_card: true,
            video_device: true,
            nfc_reader: false,
        }
    );
}

// @internal
#[test]
fn an_empty_host_tree_has_no_devices() {
    let root = tempfile::tempdir().unwrap();

    assert_eq!(
        HostDevices::under(root.path()),
        HostDevices {
            bluetooth_adapter: false,
            sound_card: false,
            video_device: false,
            nfc_reader: false,
        }
    );
}

// @internal
#[test]
fn alsas_no_soundcards_line_is_not_a_sound_card() {
    let root = tempfile::tempdir().unwrap();
    touch(root.path(), "proc/asound/cards", "--- no soundcards ---\n");
    touch(root.path(), "dev/null", "");

    let devices = HostDevices::under(root.path());

    assert!(!devices.sound_card, "{devices:?}");
    assert!(!devices.video_device, "{devices:?}");
}

// @internal
#[test]
fn a_device_counts_only_when_its_port_is_built_in() {
    let expected = DeviceCapabilities {
        has_ble: cfg!(target_os = "linux"),
        has_nfc: cfg!(feature = "nfc"),
        has_camera: cfg!(any(
            feature = "camera",
            feature = "camera-preview",
            feature = "camera-modules"
        )),
        audio: if cfg!(feature = "audio") {
            AudioCapability::Full
        } else {
            AudioCapability::None
        },
        has_internet: true,
        platform: Platform::Desktop,
        ..DeviceCapabilities::default()
    };

    assert_eq!(every_device().capabilities(), expected);
}

// @internal
#[test]
fn a_host_without_devices_reports_only_the_network() {
    let none = HostDevices {
        bluetooth_adapter: false,
        sound_card: false,
        video_device: false,
        nfc_reader: false,
    };

    assert_eq!(
        none.report(),
        Event::HardwareCapabilitiesReported {
            capabilities: DeviceCapabilities {
                has_internet: true,
                platform: Platform::Desktop,
                ..DeviceCapabilities::default()
            }
        }
    );
}
