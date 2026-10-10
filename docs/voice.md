# Voice System

The Eurus TUI voice system captures microphone audio, encodes it to Opus, sends it to the server over WebSocket, receives mixed audio from the server, decodes it, and plays it back through the system speakers. All of this happens in a dedicated tokio task that never blocks the UI render loop.

## Architecture

```
main loop (main.rs)                      VoiceManager task (voice/manager.rs)
  server messages ── VoiceCommand ──────>  session state machine
  UI state        <── VoiceEvent ────────  AudioEngine (voice/audio.rs)
                                           mic task ── audio frames ──> WebSocket
```

The main loop drains incoming WebSocket messages every tick (capped per tick so input stays responsive) and forwards voice traffic to the manager. The manager owns all voice state; the UI only mirrors it from events.

## VoiceManager (`voice/manager.rs`)

### Session states

| State | Meaning |
|---|---|
| `Idle` | Not in voice |
| `Joining` | `join_voice` sent; waiting up to 5s for the server's `voice_joined` |
| `Live` | Admitted and capturing |
| `Suspended` | WebSocket dropped mid-call; rejoin when the same room is confirmed again |

### Commands (main loop → manager)

| Command | Purpose |
|---|---|
| `Join { room_id }` | Request voice in the room |
| `Leave` | End voice, including a pending join or a suspended rejoin |
| `Mute(bool)` | Toggle microphone |
| `SetWsSender` | Socket for the current connection |
| `ConnectionStatusChanged(bool)` | WebSocket up/down |
| `RoomJoined { room_id }` | Server confirmed the chat room |
| `ServerVoiceJoined { room_id }` / `ServerVoiceError { .. }` | Server accepted/refused a voice join |
| `ServerAudio(payload)` | Mixed audio from the server |

### Events (manager → main loop)

| Event | Purpose |
|---|---|
| `Connecting` | Join requested, waiting for the server |
| `VoiceJoined`, `Connected` | Server admitted us and capture started |
| `Disconnected` | Transport lost mid-call; UI shows Reconnecting |
| `ConnectionFailed(reason)` | Join refused, timed out, or the mic failed |
| `VoiceLeft` | Voice ended |
| `MuteStateChanged(bool)` | Mute changed |
| `TxActivity(bool)` | TX indicator |
| `AudioError(String)` | Audio device or decode error |

### Lifecycle

- **Join:** send `join_voice`, enter `Joining`. On a matching `voice_joined`, start capture and go `Live`. A `voice_error` or the 5s timeout ends the attempt. The server only admits a connection that is currently in the room.
- **Capture:** each session spawns its own mic task holding that connection's socket sender and its own sequence counter, so nothing from an old session reaches a new socket. Muted frames are skipped.
- **Leave:** stop capture and playback, send `leave_voice` if the server knows about the session, clear mute, go `Idle`. Works in every state, so leaving during an outage cancels the rejoin.
- **Disconnect:** stop audio and go `Suspended`, keeping mute. On reconnect the main loop rejoins the chat room; the manager rejoins voice when `RoomJoined` names the same room.
- **Room change:** a `RoomJoined` for a different room ends voice in the old one. Navigation commands don't need their own voice cleanup.
- **Incoming audio:** played only while `Live` and only for the live room.

## AudioEngine (`voice/audio.rs`)

The AudioEngine manages low-level audio I/O using `cpal` for cross-platform audio device access and `audiopus` for Opus codec operations.

### Capture Pipeline

```
Microphone (cpal input stream)
    │
    ▼
Downmix to Mono (if stereo)
    │
    ▼
Resample to 48kHz (StatefulResampler)
    │
    ▼
Buffer to 960-sample frames (20ms at 48kHz)
    │
    ▼
Opus Encode (audiopus Encoder, VoIP mode)
    │
    ▼
Send to encoded_tx channel
```

#### Microphone Capture

`start_capture()` creates a cpal input stream on the default input device. It prefers 48kHz (Opus native rate) but falls back to whatever the device supports.

Both directions pick the best supported config at an Opus-friendly rate, preferring `f32` and falling back to `i16`/`u16` devices, converting samples to and from `f32`. Captured frames are averaged to mono. Chunks go through bounded queues; if the encoder or socket falls behind, the oldest audio is dropped instead of queued.

#### Resampling

Audio from the microphone may not be at 48kHz. The `StatefulResampler` converts between the device's sample rate and Opus's native 48kHz using linear interpolation:

```rust
let val = s0 + (s1 - s0) * self.fraction;
```

The resampler maintains state across chunks (`last_sample`, `fraction`, `samples_to_drop`) to ensure smooth transitions without clicks or gaps at chunk boundaries.

#### Encoding

A dedicated tokio task runs the Opus encoder:

```rust
let encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)?;
```

The encoder buffers resampled samples until a full Opus frame (960 samples = 20ms) is available, then encodes and sends the packet through the channel.

### Playback Pipeline

```
playback_tx channel (raw Opus packets)
    │
    ▼
Opus Decode (audiopus Decoder, 48kHz float)
    │
    ▼
Resample to device rate (StatefulResampler)
    │
    ▼
Shared buffer (VecDeque with max size)
    │
    ▼
cpal output stream callback (drains buffer)
    │
    ▼
Speakers
```

#### Decoding

The first packet from a source creates its cpal output stream and decoder task; later packets reuse it through a bounded channel:

```rust
let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)?;
```

The decoder task receives Opus packets from the `packet_rx` channel, decodes them to 48kHz float PCM, resamples to the device's native rate, and pushes samples into a shared `VecDeque` buffer.

#### Buffer Management

The shared buffer holds mono samples, capped at 500ms. If it exceeds this, the oldest samples are drained:

```rust
if buffer.len() > max_buffer_samples {
    let drain_count = buffer.len() - max_buffer_samples;
    buffer.drain(0..drain_count);
}
```

This prevents bufferbloat and keeps latency low.

#### Output

The output callback (`fill_output`) pops one mono sample per device frame and copies it to every channel, so stereo devices play at the right speed. When the buffer runs dry it writes silence.

### Stream Management

Output streams are keyed by source ID. `reset()` drops capture and playback; `drop_playback()` drops only playback so the next packet rebuilds it. A source whose stream fails to build is skipped for 2s, then retried.

The `SendStream` wrapper is necessary because `cpal::Stream` is not `Send`, but tokio requires `Send` for spawned tasks. The wrapper implements `unsafe impl Send` because the stream is only accessed from a single thread.

### Error Handling

Audio device errors are reported back to the VoiceManager via an unbounded channel. Two error types exist:

- `OutputDeviceError` — Playback device failure (speaker disconnected, driver crash)
- `InputDeviceError` — Capture device failure (microphone unplugged, permission revoked)

The VoiceManager reports both as `VoiceEvent::AudioError` and recovers:

- Output error: drop playback streams; they rebuild on the next packet.
- Input error: restart capture. If it fails, or errors again within 2s of starting, voice ends with `ConnectionFailed` rather than staying falsely Connected.

## Dependencies

- **`cpal` 0.15** — Cross-platform audio library. Uses WASAPI on Windows, ALSA/PulseAudio/PipeWire on Linux, CoreAudio on macOS.
- **`audiopus` 0.2** — Rust bindings for libopus. Requires the native `opus` library installed on the system.
- **`base64` 0.22** — Encoding/decoding Opus frames for WebSocket transport.
