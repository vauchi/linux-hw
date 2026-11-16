// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
//
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(feature = "qr-decode")]

mod qr_decode_camera_corpus {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use sha2::{Digest, Sha256};
    use vauchi_linux_hw::qr::{ScannerBackend, scan_qr_from_luma};

    fn corpus() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qr-camera-corpus")
    }

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/shared/qr_decode_camera_corpus.rs"
    ));
}
