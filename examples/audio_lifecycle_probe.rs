// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "linux", feature = "audio"))]
#[path = "audio_lifecycle_probe/measurement.rs"]
mod measurement;

#[cfg(all(target_os = "linux", feature = "audio"))]
fn main() -> std::process::ExitCode {
    use measurement::{Port, measure};
    use std::sync::Arc;
    use std::time::Duration;
    let mut complete = true;
    for port in [Port::Emit, Port::Listen] {
        println!("port={port:?} device=default playback=silence capture=discarded");
        match measure(
            Arc::new(vauchi_linux_hw::AlsaAudio),
            port,
            Duration::from_millis(250),
            Duration::from_secs(3),
        ) {
            Ok(report) => {
                println!(
                    "status={} initial_active={} replacement_return_ms={:?} backend_overlap={:?} peak_backend_calls={} unfinished_after_cleanup={} errors={:?}",
                    report.status,
                    report.initial_active,
                    report.replacement_return_ms,
                    report.backend_overlap,
                    report.peak_backend_calls,
                    report.unfinished_after_cleanup,
                    report.errors,
                );
                complete &= report.status == "measured";
            }
            Err(error) => {
                eprintln!("probe_error={error}");
                complete = false;
            }
        }
    }
    if complete {
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
