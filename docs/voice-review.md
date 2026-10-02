# Voice rejoin review

Reviewed head: `71ef1a45cbdccbfefacfc40050b32f07bcb5e42d`. Baseline: `6dca851`. Review branch: `review/voice-rejoin`.

There used to be one-sided rejoin failures where a user appeared in the call but audio stopped. The later fixes help, but they leave reproducible voice and recovery failures.

The [combined server/client review](../../eurus-server/docs/voice-review.md) records severity, source locations, reproduction evidence and fixes. Production source remains unchanged. Nothing was pushed or released.

## Local checks

```sh
cd ~/Projects/eurus-tui
~/.cargo/bin/cargo test --locked --test voice_review --test voice_room_review --no-fail-fast review::
node scripts/voice-playback-review.cjs
node scripts/voice-receive-review.cjs
```

These checks assert the desired behavior and intentionally fail on the reviewed head:

| Failure | Observation | Fix |
|---|---|---|
| Leave during outage | Rejoin intent and old room survive explicit Leave | Clear intent even when capture is already stopped |
| Muted reconnect | Disconnect preserves rejoin intent but clears mute | Preserve mute through transport cleanup and restore it in the UI |
| Invite/DM switch | Chat changes room without ending old voice capture | Reconcile voice against the server-confirmed room ID |
| Stereo output | Four mono samples fill four of eight stereo slots | Duplicate one mono sample across each output frame's channels |
| Receive throughput | Only 20 of 50 packets forwarded per simulated second | Drain incoming messages, or route bounded audio separately from UI rendering |

Ordinary leave and mono playback pass their control checks. The server's three-user real Opus leave/rejoin check also passes. The server audit separately reproduces mute-induced permanent silence, stale socket membership, an asynchronous stale join, and unauthorized private-room voice admission.

Further inspected paths need coverage: ignored voice acknowledgements, queued microphone work crossing sessions, old incoming audio without generation/room checks, reconnect from Help/RoomSwitcher, integer-only devices, device-error recovery and unbounded queues.

## Test boundaries

`tests/voice_review.rs` includes the real voice modules and seeds an active session before exercising the real state transitions. It opens no audio devices. `tests/voice_room_review.rs` includes the real application and calls its command/message handlers without running the terminal loop. Filter with `review::` to avoid unrelated included tests.

The playback script extracts and compiles the actual playback callback. Only its unused device metadata argument is stubbed. The receive script inserts the actual UI receive block into a temporary integration test, supplies virtual 50 ms ticks and real server-message handling, and removes the generated file afterward. Neither script changes application code.

The client compiles and produces an ARM64 debug binary on this machine, with existing warnings. Physical microphone/speaker behavior and full end-to-end recovery still require a two-client device test after fixes. This review has not established a native crash cause.
