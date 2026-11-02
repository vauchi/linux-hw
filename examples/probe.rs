// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! `linux-hw probe`: drives the host's hardware the way an app would and
//! prints what comes back, for rig checks against a phone (ADR-077 item 9,
//! plan Phase 2 "Tools"). It decides nothing an app would not.
//!
//! ```text
//! cargo run --example probe -- report
//! cargo run --example probe -- scan [seconds]
//! cargo run --example probe -- connect <address> [seconds]
//! cargo run --example probe -- advertise [seconds]
//! cargo run --example probe -- dial [seconds]
//! ```

use std::time::{Duration, Instant};
use vauchi_contract::{BleLinkDirection, Command, Event};
use vauchi_linux_hw::LinuxHardware;

/// Core's exchange service and payload characteristic
/// (`vauchi-core/src/exchange/ble.rs`).
const SERVICE: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
const EXCHANGE_PAYLOAD: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567891";
/// The token a phone decodes from the advertised 16-bit UUID: `abcd`.
const TOKEN: [u8; 4] = [0xab, 0xcd, 0x01, 0x02];
const DEFAULT_SECONDS: u64 = 30;
/// How often the probe drains events; an app drains on its own loop tick.
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Scan(Duration),
    Connect(String, Duration),
    Advertise(Duration),
    /// Scan, then connect to the first phone seen, as Core's central does.
    Dial(Duration),
}

fn parse(args: &[String]) -> Result<Mode, String> {
    let seconds = |arg: Option<&String>| match arg {
        None => Ok(Duration::from_secs(DEFAULT_SECONDS)),
        Some(text) => text
            .parse()
            .map(Duration::from_secs)
            .map_err(|_| format!("not a number of seconds: {text}")),
    };
    match args.first().map(String::as_str) {
        Some("report") if args.len() == 1 => Ok(Mode::Report),
        Some("scan") if args.len() <= 2 => Ok(Mode::Scan(seconds(args.get(1))?)),
        Some("connect") if (2..=3).contains(&args.len()) => {
            Ok(Mode::Connect(args[1].clone(), seconds(args.get(2))?))
        }
        Some("advertise") if args.len() <= 2 => Ok(Mode::Advertise(seconds(args.get(1))?)),
        Some("dial") if args.len() <= 2 => Ok(Mode::Dial(seconds(args.get(1))?)),
        _ => Err(
            "usage: probe report | scan [s] | connect <address> [s] | advertise [s] | dial [s]"
                .into(),
        ),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = parse(&args).unwrap_or_else(|usage| {
        eprintln!("{usage}");
        std::process::exit(2);
    });
    let mut hardware = LinuxHardware::start().unwrap_or_else(|error| {
        eprintln!("linux-hw did not start: {error}");
        std::process::exit(1);
    });
    println!("report: {:?}", hardware.report());
    let (command, duration, address) = match mode {
        Mode::Report => return,
        Mode::Scan(duration) => (
            Command::BleStartScanning {
                service_uuid: SERVICE.into(),
            },
            duration,
            None,
        ),
        Mode::Connect(address, duration) => (
            Command::BleConnect {
                device_id: address.clone(),
            },
            duration,
            Some(address),
        ),
        Mode::Advertise(duration) => (
            Command::BleStartAdvertising {
                service_uuid: SERVICE.into(),
                payload: TOKEN.to_vec(),
            },
            duration,
            None,
        ),
        Mode::Dial(duration) => (
            Command::BleStartScanning {
                service_uuid: SERVICE.into(),
            },
            duration,
            Some(String::new()),
        ),
    };
    run(&mut hardware, command, duration, address.as_deref());
}

/// `peer` is the address to read from once connected; an empty one means
/// "the first phone the scan sees", which the probe then dials.
fn run(hardware: &mut LinuxHardware, command: Command, duration: Duration, peer: Option<&str>) {
    println!("execute: {command:?}");
    if let Err(error) = hardware.execute(command) {
        eprintln!("refused: {error}");
        return;
    }
    let mut peer = peer.map(String::from);
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        while let Some(event) = hardware.pop() {
            println!("event: {}", summary(&event));
            match (&event, peer.as_deref()) {
                (Event::BleAdvertisementObserved(seen), Some("")) => {
                    peer = Some(seen.id().to_string());
                    dial(hardware, seen.id());
                }
                (Event::BleConnected { device_id, .. }, Some(peer)) => {
                    read_payload(hardware, device_id, peer);
                }
                _ => {}
            }
        }
        std::thread::sleep(POLL);
    }
    println!("done after {}s", duration.as_secs());
}

fn dial(hardware: &mut LinuxHardware, device_id: &str) {
    for command in [
        Command::BleStopScanning,
        Command::BleConnect {
            device_id: device_id.into(),
        },
    ] {
        println!("execute: {command:?}");
        if let Err(error) = hardware.execute(command) {
            eprintln!("refused: {error}");
        }
    }
}

/// What Core does first on an outbound link: read the peer's payload.
fn read_payload(hardware: &mut LinuxHardware, device_id: &str, peer: &str) {
    if device_id != peer {
        return;
    }
    let read = Command::BleReadCharacteristic {
        device_id: device_id.into(),
        direction: BleLinkDirection::Outbound,
        uuid: EXCHANGE_PAYLOAD.into(),
    };
    println!("execute: {read:?}");
    if let Err(error) = hardware.execute(read) {
        eprintln!("refused: {error}");
    }
}

/// Byte payloads as their length and first bytes, so a card does not
/// scroll the terminal.
fn summary(event: &Event) -> String {
    let bytes = |data: &[u8]| format!("{} bytes {:02x?}", data.len(), &data[..data.len().min(8)]);
    match event {
        Event::BleCharacteristicRead { uuid, data, .. } => format!("read {uuid}: {}", bytes(data)),
        Event::BleCharacteristicNotified {
            uuid,
            data,
            direction,
            ..
        } => {
            format!("notified {direction:?} {uuid}: {}", bytes(data))
        }
        other => format!("{other:?}"),
    }
}

// INLINE_TEST_REQUIRED: an example's private parser is reachable only from inside it.
#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    // @internal
    #[test]
    fn each_mode_parses_with_its_default_or_given_duration() {
        let default = Duration::from_secs(DEFAULT_SECONDS);
        assert_eq!(parse(&args("report")), Ok(Mode::Report));
        assert_eq!(parse(&args("scan")), Ok(Mode::Scan(default)));
        assert_eq!(
            parse(&args("scan 5")),
            Ok(Mode::Scan(Duration::from_secs(5)))
        );
        assert_eq!(
            parse(&args("connect AA:BB:CC:DD:EE:FF 9")),
            Ok(Mode::Connect(
                "AA:BB:CC:DD:EE:FF".into(),
                Duration::from_secs(9)
            ))
        );
        assert_eq!(parse(&args("advertise")), Ok(Mode::Advertise(default)));
        assert_eq!(
            parse(&args("dial 7")),
            Ok(Mode::Dial(Duration::from_secs(7)))
        );
    }

    // @internal
    #[test]
    fn anything_else_is_the_usage() {
        for wrong in ["", "scan x", "connect", "report 3", "scan 1 2", "listen"] {
            assert!(
                parse(&args(wrong)).unwrap_err().starts_with("usage") || wrong == "scan x",
                "{wrong:?}"
            );
        }
        assert_eq!(
            parse(&args("scan x")),
            Err("not a number of seconds: x".into())
        );
    }
}
