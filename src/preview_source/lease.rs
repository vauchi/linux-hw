// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::PreviewDirectory;

pub struct PreviewLease {
    pub(super) directory: PreviewDirectory,
    pub(super) generation: u64,
}

impl Drop for PreviewLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.directory.0.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|entry| entry.generation == self.generation)
        {
            state.active = None;
        }
    }
}
