// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The PC/SC reader behind the NFC port (USB NFC readers through
//! pcsclite), moved from linux-gtk's `platform/nfc.rs`.
use crate::ports::{NfcCard, NfcReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// PC/SC offers no tag-arrival callback, so the wait polls; the interval
/// only bounds how late a deactivation is noticed.
const TAG_POLL: Duration = Duration::from_millis(200);

pub struct PcscReader;

struct PcscCard(pcsc::Card);

impl NfcCard for PcscCard {
    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, String> {
        let mut response = vec![0u8; pcsc::MAX_BUFFER_SIZE_EXTENDED];
        self.0
            .transmit(apdu, &mut response)
            .map(<[u8]>::to_vec)
            .map_err(|e| e.to_string())
    }
}

impl NfcReader for PcscReader {
    fn wait_for_tag(&self, cancelled: &AtomicBool) -> Result<Box<dyn NfcCard>, String> {
        let context = pcsc::Context::establish(pcsc::Scope::System).map_err(|e| e.to_string())?;
        while !cancelled.load(Ordering::SeqCst) {
            let mut names = vec![0u8; context.list_readers_len().map_err(|e| e.to_string())?];
            for reader in context
                .list_readers(&mut names)
                .map_err(|e| e.to_string())?
            {
                if let Ok(card) =
                    context.connect(reader, pcsc::ShareMode::Shared, pcsc::Protocols::ANY)
                {
                    return Ok(Box::new(PcscCard(card)));
                }
            }
            std::thread::sleep(TAG_POLL);
        }
        Err("NFC reader deactivated".into())
    }
}
