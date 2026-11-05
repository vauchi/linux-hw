<!-- SPDX-FileCopyrightText: 2026 Mattia Egloff <mattia.egloff@pm.me> -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Audio lifecycle probe

Measures replacement through `AudioSession` and the real `AlsaAudio`
backend. Playback uses silent samples; capture briefly reads the default
microphone and discards its samples without writing them to disk. Both
directions use ALSA's `default` route, respecting the desktop audio server.

From a Vauchi workspace root on Linux:

```bash
just exec-in-repo linux-hw \
  'cargo build --locked --features audio --example audio_lifecycle_probe'
timeout --kill-after=3s 45s just exec-in-repo linux-hw \
  'target/debug/examples/audio_lifecycle_probe'
```

Build before applying the process timeout. No privileged execution or
raw-device takeover is needed. Keep the full output, including ALSA errors.

- `initial_active`: the first backend call was still running after the
  settling interval. This does not establish that a physical PCM opened.
- `returned_before_stop`: the first call returned during the replacement
  observation window, before cleanup requested `AudioStop`.
- `replacement_return_ms`: elapsed time from requesting replacement to
  the first backend's return. It includes request setup and scheduling;
  it is not a direct timestamp of the device noticing cancellation.
  It is absent when the return occurs only during cleanup.
- `backend_overlap`: whether the second backend call entered before the
  first returned. It does not assert simultaneous physical PCM ownership.
- `peak_backend_calls`: maximum overlapping instrumented invocations.
- `unfinished_after_cleanup`: calls still unreturned after bounded cleanup.
- `errors`: backend and shutdown errors, without captured samples.

Exit zero means both measurements completed, even if overlap was observed;
it is not a native-device acceptance verdict. Exit one means inconclusive,
for example when the device failed to open or returned before replacement.
An external timeout is also inconclusive. Do not infer safe latency or
exclusive device ownership from an incomplete run.

The probe requests up to ten seconds of audio per call, waits 250 ms before
replacement, observes replacement for up to three seconds, then requests
stop and observes cleanup for up to three seconds. An outer process
timeout protects against an unexpected synchronous hang. It measures
backend return, not downstream audio-server buffering or acoustic silence.

Tests inject delayed exit, device refusal, and an operation that returns
only during cleanup. The last case must remain inconclusive, so cleanup
cannot create a false replacement success.

## Locating an unfinished native call

Set `VAUCHI_AUDIO_TRACE=1` to enable stage timing in the existing ALSA
backend without `strace`:

```bash
VAUCHI_AUDIO_TRACE=1 timeout --kill-after=3s 45s \
  just exec-in-repo linux-hw 'target/debug/examples/audio_lifecycle_probe'
```

Run from the authorized desktop audio session, on AC. Build the example
first as above. This does not change device permissions or ALSA settings.
The flag is off unless its value is exactly `1`.

The additional stderr records contain only a call ID, port, stage,
entry/return edge and milliseconds elapsed since that call began. They
contain no samples, paths, device payloads or encoded contact data.
Call IDs distinguish overlapping operations. A `return` edge means the
stage returned, including an error; it does not mean success.

Stages cover `operation`, `open`, `configure`, `io`, `write` or `read`,
`start` for capture, `recover`, `retry_write`, `finish`, `drop` and `close`.
The read/write stages cover their whole loops rather than each chunk.
Recovery appears only when needed. Close surrounds the actual PCM
destructor, including error paths after a successful open. The enclosing
operation returns only after that cleanup. A failed open has no close.

For a call still unfinished at the probe deadline, retain its last entry
without a matching return. This identifies a region to investigate, not
proof of permanent deadlock. Stage output also changes timing and can
wait on stderr; keep the output consumer draining and verify a fix with
the ordinary probe afterward. Do not treat a passing diagnostic run as
evidence that an earlier unfinished call is fixed.

The stage tests run the real backend in subprocesses with a temporary
ALSA null-device configuration or a deliberately unavailable plugin.
They verify diagnostic boundaries and unchanged API results without
accessing host audio. These controls are not native-device acceptance.

## Playback completion and cancellation

Playback now observes delay and stream state with cancellation checks
instead of calling the native blocking drain function. A prepared stream is
started before waiting, including a short buffer that did not start
automatically. Cancellation stops waiting and explicitly drops queued
frames; ordinary completion waits for nonpositive delay or an exhausted
stream before dropping and closing the PCM. Delay includes fixed device
latency and need not reach zero after underrun, so the PCM's underrun
state also establishes completion even when the delay query succeeds
with a positive value. Start, delay-query and stop errors reach the caller.

The `finish` stage covers this wait, followed by a separate `drop` stage
for the native stop call. The loop pauses for 10 ms between queries;
this is not a bound on cancellation latency because native queries,
device setup and stop/close calls can still block. Both replacement and
cleanup still need the physical probe, followed by an ordinary run with
stage output disabled. No acoustic-silence guarantee follows from API return.

The distinction between preserving pending frames with drain and
discarding them with drop is documented in the
[ALSA PCM reference](https://www.alsa-project.org/alsa-doc/alsa-lib/group___p_c_m.html).
The upstream [PulseAudio ALSA plugin](https://github.com/alsa-project/alsa-plugins/blob/master/pulse/pcm_pulse.c)
implements drain with a synchronous operation wait, so setting a
nonblocking flag alone is not a sufficient fix for that path.
