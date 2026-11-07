// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "linux", feature = "audio"))]
#[path = "audio_lifecycle_probe/acceptance.rs"]
mod acceptance;
#[cfg(all(target_os = "linux", feature = "audio"))]
#[path = "audio_lifecycle_probe/trace.rs"]
mod trace;

#[cfg(all(target_os = "linux", feature = "audio"))]
fn main() -> std::process::ExitCode {
    use acceptance::{Mode, measure, run_modes};
    use std::sync::Arc;
    use std::time::Duration;

    let success = run_modes(|mode| {
        println!(
            "mode={mode:?} device=default playback=silence capture=discarded requested_playback_ms={}",
            if mode == Mode::Completion {
                1000
            } else {
                10000
            }
        );
        match measure(
            Arc::new(vauchi_linux_hw::AlsaAudio),
            mode,
            Duration::from_millis(250),
            Duration::from_secs(3),
            || eprintln!("audio_observation mode={mode:?}"),
        ) {
            Ok(report) => {
                println!(
                    "mode={mode:?} status={} active_at_observation={} natural_return={} return_ms={:?} unfinished_after_cleanup={} errors={:?}",
                    report.status,
                    report.active_at_observation,
                    report.natural_return,
                    report.return_ms,
                    report.unfinished_after_cleanup,
                    report.errors
                );
                Ok(report)
            }
            Err(error) => {
                eprintln!("probe_error={error}");
                Err(error)
            }
        }
    });
    if success {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

#[cfg(not(all(target_os = "linux", feature = "audio")))]
fn main() -> std::process::ExitCode {
    eprintln!("This probe requires Linux and --features audio.");
    std::process::ExitCode::FAILURE
}
