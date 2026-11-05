// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(target_os = "linux", feature = "audio"))]

use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use vauchi_linux_hw::{AlsaAudio, AudioBackend};

fn run_worker(trace: &str, invalid_device: bool) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("alsa.conf");
    let plugin = if invalid_device {
        "vauchi_missing_plugin"
    } else {
        "null"
    };
    std::fs::write(&config, format!("pcm.!default {{ type {plugin} }}\n")).unwrap();
    Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "diagnostic_worker", "--nocapture"])
        .env("ALSA_CONFIG_PATH", config)
        .env("VAUCHI_AUDIO_TRACE", trace)
        .env(
            "VAUCHI_DIAGNOSTIC_WORKER",
            if invalid_device { "invalid" } else { "null" },
        )
        .output()
        .unwrap()
}

fn stages(output: &Output, port: &str) -> Vec<(String, String)> {
    String::from_utf8(output.stderr.clone())
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("audio_stage ") && line.contains(&format!("port={port} ")))
        .map(|line| {
            let field = |name: &str| {
                line.split_whitespace()
                    .find_map(|value| value.strip_prefix(name))
                    .unwrap()
                    .to_owned()
            };
            let elapsed: f64 = field("elapsed_ms=").parse().unwrap();
            assert!(elapsed.is_finite() && elapsed >= 0.0);
            let _: u64 = field("call=").parse().unwrap();
            (field("stage="), field("edge="))
        })
        .collect()
}

fn paired(names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .flat_map(|name| {
            [
                (name.to_string(), "enter".to_owned()),
                (name.to_string(), "return".to_owned()),
            ]
        })
        .collect()
}

#[test]
fn enabled_trace_reports_both_operations_through_pcm_cleanup() {
    let output = run_worker("1", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (port, operations) in [
        (
            "Emit",
            vec!["open", "configure", "io", "write", "drain", "close"],
        ),
        (
            "Listen",
            vec!["open", "configure", "io", "start", "read", "close"],
        ),
    ] {
        let mut expected = vec![("operation".into(), "enter".into())];
        expected.extend(paired(&operations));
        expected.push(("operation".into(), "return".into()));
        assert_eq!(stages(&output, port), expected);
    }
}

#[test]
fn disabled_trace_preserves_results_without_diagnostic_output() {
    let output = run_worker("0", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn failed_open_returns_from_the_stage_without_claiming_a_close() {
    let output = run_worker("1", true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stages(&output, "Emit"),
        vec![
            ("operation".into(), "enter".into()),
            ("open".into(), "enter".into()),
            ("open".into(), "return".into()),
            ("operation".into(), "return".into()),
        ]
    );
}

#[test]
#[ignore = "parent tests supply an isolated ALSA configuration"]
fn diagnostic_worker() {
    let mode = std::env::var("VAUCHI_DIAGNOSTIC_WORKER").unwrap();
    let cancelled = AtomicBool::new(false);
    if mode == "invalid" {
        let error = AlsaAudio
            .emit(&[0.0; 2048], 48_000, &cancelled)
            .unwrap_err();
        assert!(error.starts_with("ALSA open failed:"), "{error}");
    } else {
        assert_eq!(mode, "null");
        assert_eq!(AlsaAudio.emit(&[0.0; 2048], 48_000, &cancelled), Ok(()));
        assert_eq!(
            AlsaAudio.record(Duration::ZERO, &cancelled),
            Ok((Vec::new(), 48_000))
        );
    }
}
