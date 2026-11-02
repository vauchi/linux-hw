// SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `qr::scan_qr_from_luma` decodes with `rqrr`, whose only crash guard is
//! the clamped `Perspective::map` of the fork in `vendor/rqrr`. The
//! `[patch.crates-io]` stanza redirects the registry dependency to that
//! path; this test reads the lock file and fails the moment `rqrr`
//! resolves from the registry again. Moved from linux-gtk, which now
//! reads the decoder from this crate. Record:
//! `_private/docs/problems/done/2026-09-10-linux-gtk-unpatched-rqrr-abort-path/`.
const LOCK: &str = include_str!("../Cargo.lock");

/// Every `rqrr` package in the lock file with its `source`, `None` for a
/// path dependency, which the lock file records without one.
fn rqrr_sources() -> Vec<Option<String>> {
    let mut sources = Vec::new();
    let mut current: Option<Option<String>> = None;
    for line in LOCK
        .lines()
        .map(str::trim)
        .chain(std::iter::once("[[package]]"))
    {
        if line == "[[package]]" {
            if let Some(source) = current.take() {
                sources.push(source);
            }
        } else if line == "name = \"rqrr\"" {
            current = Some(None);
        } else if let (Some(slot), Some(rest)) = (current.as_mut(), line.strip_prefix("source = "))
        {
            *slot = Some(rest.trim_matches('"').to_string());
        }
    }
    sources
}

// @scenario: exchange :: A malformed QR frame never aborts the desktop shell
#[test]
fn rqrr_resolves_from_the_vendored_fork_not_the_registry() {
    let sources = rqrr_sources();
    assert_eq!(
        sources.len(),
        1,
        "exactly one rqrr must resolve; two means the patch left a dead second entry: {sources:?}"
    );
    assert_eq!(
        sources[0], None,
        "rqrr must come from vendor/rqrr (a path patch has no source line)"
    );
}
