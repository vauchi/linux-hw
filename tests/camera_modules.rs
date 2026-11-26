// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(target_os = "linux", feature = "camera-modules"))]

use std::{os::unix::fs::PermissionsExt, path::Path};
use vauchi_linux_hw::camera_modules::{BackendChoice, CameraModules};

fn install(directory: &Path, name: &str) {
    let path = directory.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// @internal — installed providers and sandbox boundaries.
#[test]
fn absent_or_disabled_providers_leave_camera_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    assert!(CameraModules::discover_in(dir.path(), BackendChoice::Auto, false).is_none());
    install(dir.path(), "vauchi-camera-pipewire");
    assert!(CameraModules::discover_in(dir.path(), BackendChoice::Disabled, false).is_none());
}

// @internal
#[test]
fn installed_providers_and_explicit_selection_choose_one_backend() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "vauchi-camera-v4l2");
    let direct = CameraModules::discover_in(dir.path(), BackendChoice::Auto, false).unwrap();
    assert_eq!(direct.choice(), BackendChoice::Direct);
    assert!(CameraModules::discover_in(dir.path(), BackendChoice::PipeWire, false).is_none());
    install(dir.path(), "vauchi-camera-pipewire");
    let automatic = CameraModules::discover_in(dir.path(), BackendChoice::Auto, false).unwrap();
    assert_eq!(automatic.choice(), BackendChoice::PipeWire);
    let direct = CameraModules::discover_in(dir.path(), BackendChoice::Direct, false).unwrap();
    assert_eq!(direct.choice(), BackendChoice::Direct);
    assert!(automatic.available());
}

// @internal
#[test]
fn flatpak_never_selects_a_direct_provider() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "vauchi-camera-v4l2");
    assert!(CameraModules::discover_in(dir.path(), BackendChoice::Auto, true).is_none());
    assert!(CameraModules::discover_in(dir.path(), BackendChoice::Direct, true).is_none());
    install(dir.path(), "vauchi-camera-pipewire");
    assert_eq!(
        CameraModules::discover_in(dir.path(), BackendChoice::Auto, true)
            .unwrap()
            .choice(),
        BackendChoice::PipeWire
    );
}

// @internal
#[test]
fn provider_configuration_rejects_unknown_values_and_paths() {
    assert_eq!(
        "direct".parse::<BackendChoice>().unwrap(),
        BackendChoice::Direct
    );
    for value in ["", "/tmp/helper", "direct; echo surprise", "PIPEWIRE"] {
        assert!(value.parse::<BackendChoice>().is_err());
    }
}
