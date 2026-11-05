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
