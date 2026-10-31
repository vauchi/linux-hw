<!-- SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me> -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# linux-hw

Vauchi's hardware shell for Linux. It executes the hardware commands
[Vauchi Core](https://gitlab.com/vauchi/core) emits and reports what the
OS observed as events, through BlueZ over D-Bus, PC/SC, PipeWire and
mDNS. It decides nothing: Core chooses peers, roles, framing, deadlines
and fallback. It shows no UI: the app's display shell renders everything
the user sees.

It serves the GTK, Qt, CLI and TUI apps.

## Contents (planned)

- one port per hardware capability: BLE, NFC, camera, audio, direct send
- contract tests against Core's command/event fixtures, run on a fake OS
  backend in CI
- real-device tests, a capability probe, measurement tools and
  diagnostics

## License

AGPL-3.0-or-later; see `LICENSE`.
