# Voice System

The Eurus TUI voice system captures microphone audio, encodes it to Opus, sends it to the server over WebSocket, receives mixed audio from the server, decodes it, and plays it back through the system speakers. All of this happens in a dedicated tokio task that never blocks the UI render loop.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        VoiceManager Task                             │
│                                                                      │
│  ┌─────────────┐    ┌─────────────────────────────────────────────┐ │
│  │  Commands   │    │              VoiceManager                    │ │
│  │  (from main)│───>│  - join_voice / leave_voice / mute / signal │ │
│  │             │    │  - is_joined / is_muted state               │ │
│  └─────────────┘    └──────────┬──────────────────┬────────────────┘ │
│                                │                  │                   │
│                    ┌───────────▼──────┐  ┌────────▼───────────────┐  │
│                    │   AudioEngine    │  │   playback_tx channel  │  │
│                    │   (audio.rs)     │  │   (for incoming audio) │  │
│                    └──────────────────┘  └────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
         │                                        │
         │  VoiceEvent::Signal { audio }          │  VoiceEvent::Signal { audio }
         ▼                                        ▼
┌────────────────────┐                  ┌────────────────────────────┐
│  Main Loop         │                  │  AudioEngine               │
│  (main.rs)         │                  │  (voice/audio.rs)          │
│                    │                  │                            │
│  Forwards Signal   │                  │  decode → resample → cpal  │
│  events over       │                  │  playback                  │
│  WebSocket         │                  │                            │
└────────────────────┘                  └────────────────────────────┘
```

## VoiceManager (`voice/manager.rs`)

The VoiceManager runs as a dedicated tokio task spawned during application initialization. It communicates with the main loop through two unbounded channels:

### Commands (Main Loop → VoiceManager)

| Command | Purpose |
|---|---|
| `Join(room_id)` | Start a voice session in the given room |
| `Leave` | End the current voice session |
| `Mute(bool)` | Toggle microphone on/off |
| `Signal { sender_id, signal_type, data }` | Forward a voice signal received from the server |

### Events (VoiceManager → Main Loop)

| Event | Purpose |
|---|---|
| `Signal { ... }` | Audio data or control signal to send to the server |
| `Connecting` | Voice connection initiated |
| `Connected` | Voice connection established |
| `Disconnected` | Voice connection closed |
| `ConnectionFailed(reason)` | Connection failed with error message |
| `MuteStateChanged(bool)` | Mute state updated |
| `TxActivity(bool)` | Microphone activity detected (for TX indicator) |
| `AudioError(String)` | Audio device error |

### Join Flow

1. Send `VoiceEvent::Connecting` to update UI
2. Reset the AudioEngine (stop any existing streams)
3. Set `is_joined = true` and store the room ID
4. Start microphone capture via `AudioEngine.start_capture()`
5. Spawn a task that reads encoded Opus packets from the capture channel:
   - If muted, skip the packet
   - Otherwise, base64-encode the Opus data and emit a `Signal` event with `type: "audio"`
   - Emit `TxActivity(true)` to show the TX indicator
6. Set up a playback channel (`playback_tx`) and start playback via `AudioEngine.start_playback_for_peer("server", playback_rx)`
7. Send a `join_voice` signal to the server
8. Send `VoiceEvent::Connected` to confirm the connection

### Leave Flow

1. Set `is_joined = false`
2. Send a `leave_voice` signal to the server
3. Reset the AudioEngine (stops all cpal streams)
4. Clear the playback channel
5. Reset mute state
6. Send `TxActivity(false)`, `MuteStateChanged(false)`, and `Disconnected` events

### Signal Handling

The VoiceManager receives signals from the server (forwarded by the main loop). The only signal type it processes is:

- **`audio`** — Base64-decode the data and send the raw Opus packet to the `playback_tx` channel. The AudioEngine's decoder task picks it up and plays it through the speakers.

All other signal types are ignored (they were used by the old WebRTC architecture).

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

The cpal callback receives audio chunks in the device's native format. If the device is stereo, channels are averaged to mono:

```rust
let mono: Vec<f32> = data.chunks(channels)
    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
    .collect();
```

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

Each `start_playback_for_peer()` call creates a new cpal output stream with its own decoder task:

```rust
let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)?;
```

The decoder task receives Opus packets from the `packet_rx` channel, decodes them to 48kHz float PCM, resamples to the device's native rate, and pushes samples into a shared `VecDeque` buffer.

#### Buffer Management

The shared buffer has a maximum size of 2 seconds of audio (`device_sample_rate * 2`). If the buffer exceeds this limit, the oldest samples are drained:

```rust
if buffer.len() > max_buffer_samples {
    let drain_count = buffer.len() - max_buffer_samples;
    buffer.drain(0..drain_count);
}
```

This prevents bufferbloat and keeps latency low.

#### Output

The cpal output callback drains samples from the shared buffer. If the buffer is empty, it fills the remainder with silence:

```rust
for i in written..data.len() {
    data[i] = 0.0;
}
```

This ensures the audio device always has data, preventing underrun glitches.

### Stream Management

Output streams are keyed by peer ID in a `HashMap<String, SendStream>`. When `start_playback_for_peer()` is called for a peer that already has a stream, the old stream is replaced. When `reset()` is called, all streams are dropped.

The `SendStream` wrapper is necessary because `cpal::Stream` is not `Send`, but tokio requires `Send` for spawned tasks. The wrapper implements `unsafe impl Send` because the stream is only accessed from a single thread.

### Error Handling

Audio device errors are reported back to the VoiceManager via an unbounded channel. Two error types exist:

- `OutputDeviceError` — Playback device failure (speaker disconnected, driver crash)
- `InputDeviceError` — Capture device failure (microphone unplugged, permission revoked)

The VoiceManager forwards these as `VoiceEvent::AudioError` events, which the main loop displays as status messages.

## Dependencies

- **`cpal` 0.15** — Cross-platform audio library. Uses WASAPI on Windows, ALSA/PulseAudio/PipeWire on Linux, CoreAudio on macOS.
- **`audiopus` 0.2** — Rust bindings for libopus. Requires the native `opus` library installed on the system.
- **`base64` 0.22** — Encoding/decoding Opus frames for WebSocket transport.
