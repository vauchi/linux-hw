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

## Contents

The crate `vauchi-linux-hw` (ADR-078 step 2.1; it was Core's
`vauchi-hardware`): the BlueZ discovery and delivery ports over D-Bus,
the host executor that runs them, and a fake BlueZ backend (`fake-bluez`
feature) for the tests CI runs without radios. It depends on the shell
contract (`vauchi-contract`) and on nothing from Core.

Planned next: PC/SC NFC, PipeWire camera capture with a preview stream,
audio proximity, mDNS/TCP direct send; contract tests against the
command/event fixtures; real-device tests, a capability probe,
measurement tools and diagnostics.

## License

AGPL-3.0-or-later; see `LICENSE`.
