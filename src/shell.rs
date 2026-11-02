// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::advertising::BleAdvertiser;
use crate::discovery::BleScanner;
use crate::ports::{ExecutorError, Notification};
use vauchi_contract::executor::DeliveryError;
use vauchi_contract::{BleLinkDirection, Command, Event};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecuteError {
    /// linux-hw has no port for this command on this host. The shell
    /// reports nothing: its capability report tells Core what it runs
    /// (owner decision 2026-11-02, private#770).
    #[error("this hardware shell cannot run the command")]
    Unsupported,
    #[error(transparent)]
    Delivery(#[from] DeliveryError),
}

/// The entry point an app hands Core's hardware commands to (ADR-077
/// item 4, ADR-078 item 3). Each command goes to the executor that owns
/// its port; the shell decides nothing beyond that.
#[derive(Default)]
pub struct HardwareShell {
    scanner: Option<BleScanner>,
    advertiser: Option<BleAdvertiser>,
    scanner_pops_next: bool,
}

impl HardwareShell {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_scanner(mut self, scanner: BleScanner) -> Self {
        self.scanner = Some(scanner);
        self
    }

    pub fn with_advertiser(mut self, advertiser: BleAdvertiser) -> Self {
        self.advertiser = Some(advertiser);
        self
    }

    pub fn execute(&mut self, command: Command) -> Result<(), ExecuteError> {
        match command {
            Command::BleStartScanning { service_uuid } => {
                Ok(self.scanner()?.start(service_uuid)?)
            }
            Command::BleStopScanning => {
                self.scanner()?.stop();
                Ok(())
            }
            Command::BleStartAdvertising {
                service_uuid,
                payload,
            } => Ok(self.advertiser()?.start(service_uuid, payload)?),
            Command::BleStopAdvertising => {
                self.advertiser()?.stop();
                Ok(())
            }
            Command::BleWriteCharacteristic {
                device_id,
                direction: BleLinkDirection::Inbound,
                uuid,
                data,
            } => Ok(self.advertiser()?.notify(Notification {
                device_id,
                uuid,
                data,
            })?),
            _ => Err(ExecuteError::Unsupported),
        }
    }

    /// The next observation from any executor. Alternates between them so
    /// a busy scan cannot starve the advertiser's events.
    pub fn pop(&mut self) -> Option<Event> {
        self.scanner_pops_next = !self.scanner_pops_next;
        if self.scanner_pops_next {
            self.pop_scanner().or_else(|| self.pop_advertiser())
        } else {
            self.pop_advertiser().or_else(|| self.pop_scanner())
        }
    }

    /// Withdraw every running operation and wait until each executor has
    /// dropped its native future.
    pub async fn shutdown(&mut self) -> Result<(), ExecutorError> {
        if let Some(scanner) = &mut self.scanner {
            scanner.shutdown().await?;
        }
        if let Some(advertiser) = &mut self.advertiser {
            advertiser.shutdown().await?;
        }
        Ok(())
    }

    fn scanner(&mut self) -> Result<&mut BleScanner, ExecuteError> {
        self.scanner.as_mut().ok_or(ExecuteError::Unsupported)
    }

    fn advertiser(&mut self) -> Result<&mut BleAdvertiser, ExecuteError> {
        self.advertiser.as_mut().ok_or(ExecuteError::Unsupported)
    }

    fn pop_scanner(&mut self) -> Option<Event> {
        self.scanner.as_mut().and_then(BleScanner::pop)
    }

    fn pop_advertiser(&mut self) -> Option<Event> {
        self.advertiser.as_mut().and_then(BleAdvertiser::pop)
    }
}
