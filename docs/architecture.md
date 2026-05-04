# Architecture

## Application Structure

Eurus TUI is built around a single `App` struct that holds all application state. The application runs on a tokio multi-threaded runtime and uses a screen-based navigation model where each screen has its own input handler and rendering logic.

```
                    ┌─────────────────────────────────────┐
                    │         Terminal (crossterm)         │
                    │         Raw mode, alternate screen    │
                    └──────────────────┬──────────────────┘
                                       │
                    ┌──────────────────▼──────────────────┐
                    │         UI Rendering (ratatui)        │
                    │         Frame-based, 60fps target     │
                    └──────────────────┬──────────────────┘
                                       │
                    ┌──────────────────▼──────────────────┐
                    │         App State (main.rs)           │
                    │         Centralized state container   │
                    └──────┬───────────────┬───────────────┘
                           │               │
              ┌────────────┼───────────────┼───────────────┐
              │            │               │               │
    ┌─────────▼──────┐ ┌──▼────────┐ ┌───▼────────┐ ┌───▼────────────┐
    │ WebSocket Layer │ │ Voice     │ │ HTTP API   │ │ Submodules     │
    │ (tokio tasks)   │ │ System    │ │ (reqwest)  │ │ - crypto       │
    │ - incoming      │ │ (manager) │ │ - register │ │ - ssh          │
    │ - outgoing      │ │ (audio)   │ │ - challenge│ │ - config       │
    │ - ping/pong     │ │           │ │ - verify   │ │ - emoji        │
    │ - reconnect     │ │           │ │ - refresh  │ │ - clipboard    │
    └─────────────────┘ └───────────┘ └────────────┘ └────────────────┘
```

## Screen Flow

The application moves through a series of screens based on user input and authentication state:

```
[Startup]
    │
    ├── [No token?] ──yes──> [No SSH keys?] ──yes──> [Registration]
    │                           │                         │
    │                          no                         │ (press r to retry)
    │                           │                         │
    │                    [KeySelection] ───────────────────┘
    │                         │
    │                         v
    │                   [UsernameInput] ──> [User exists?] ──yes──> [Challenge/Sign]
    │                         │                    │                     │
    │                         │                   no                     │
    │                         │                    │                     │
    │                         │              [Register] <────────────────┘
    │                         │                    │
    │                         │                    v
    │                         └──────────> [RegistrationSuccess]
    │
    └── [Token exists] ──────────────────────────────────────┘
                              │
                              v
                        [RoomChoice]
                         │      │
                    c    │      │  j (join by list)
                    v    │      │  v
            [RoomTypeSelection]  [RoomList]
                    │            │
                    v            │  (select room, Enter)
            [CreateRoomInput]    │
                    │            │
                    v            │
            [RoomCreation]       │
                    │            │
                    └─────┬──────┘
                          │
                          v
                       [InRoom]
                    (main chat screen)
```

### In-Room Overlays

While in the `InRoom` screen, several overlays can be activated:

| Overlay | Trigger | Description |
|---|---|---|
| Room Switcher | `:list` / `:l` | Centered box showing user's rooms |
| Help | `:help` / `:h` | Full-screen help text |
| User List | `:users` / `:u` | Centered box with online users + typing indicators |
| Emoji Picker | `:` in Insert mode | Floating box above input with matching emojis |

## WebSocket Connection Management

### Connection Establishment

1. Load auth token from `~/.config/eurus/token`
2. Append token as query parameter: `wss://host/ws?token=<jwt>`
3. Connect via `tokio_tungstenite::connect_async`
4. Split the stream into read and write halves
5. Store the write half's sender in `app.ws_sender`

### Incoming Task (Spawned)

A dedicated tokio task reads from the WebSocket:

```rust
tokio::spawn(async move {
    while let Some(msg) = ws_reader.next().await {
        if let Ok(text) = msg.into_text() {
            let _ = ws_incoming_tx.send(text);
        }
    }
    let _ = ws_incoming_tx.send("__DISCONNECT__".to_string());
});
```

Messages are sent to the main loop via an unbounded channel. On disconnect, a sentinel value `"__DISCONNECT__"` is sent.

### Outgoing Task (Spawned)

A separate task handles outgoing messages and periodic pings:

```rust
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            Some(msg) = ws_outgoing_rx.recv() => {
                let _ = ws_writer.send(Message::Text(msg)).await;
            }
            _ = interval.tick() => {
                let _ = ws_writer.send(Message::Ping(vec![])).await;
            }
        }
    }
});
```

### Reconnection

On disconnect, the main loop detects `__DISCONNECT__` and attempts reconnection with exponential backoff:

- Up to 5 retry attempts
- Backoff: 1s, 2s, 4s, 8s, 16s (capped at 30s)
- On successful reconnect, re-joins the current room automatically

### Token Auto-Refresh

If the current token expires within 24 hours, the client silently refreshes it via `POST /api/auth/refresh` before it expires. This prevents unexpected disconnections.

## Voice System Architecture

Voice runs as a dedicated tokio task separate from the main UI loop:

```
[Main Loop] <--VoiceEvent-- [VoiceManager Task] <--AudioDeviceError-- [AudioEngine]
     │                              │
     │---VoiceCommand-------------->│
     │
     +-- (forward Signal events over WebSocket)
```

### Command Channel

The main loop sends commands to the VoiceManager:

- `Join(room_id)` — Start voice session
- `Leave` — End voice session
- `Mute(bool)` — Toggle microphone
- `Signal { sender_id, signal_type, data }` — Forward server voice signals

### Event Channel

The VoiceManager sends events back to the main loop:

- `Signal { ... }` — Audio data or control signals to forward to server
- `Connecting` / `Connected` / `Disconnected` — Connection state changes
- `ConnectionFailed(reason)` — Error with description
- `MuteStateChanged(bool)` — Mute toggle confirmation
- `TxActivity(bool)` — Microphone activity indicator
- `AudioError(String)` — Audio device errors

This separation ensures that audio processing (which involves blocking cpal callbacks) never blocks the UI render loop.

## Command System

Commands are entered by pressing `:` in any screen. The `execute_command` function parses the input and dispatches to the appropriate handler. Commands can:

- Navigate between screens (`:q`, `:list`)
- Manage rooms (`:rename`, `:delete`, `:transfer`, `:share`, `:j`)
- Control voice (`:vc`, `:m`, `:um`, `:vcl`)
- Toggle UI elements (`:users`, `:help`)
- Start DMs (`:dm`)
- Re-register (`:register`)

See [Commands](commands.md) for the complete reference.

## Vim Mode

The editor uses a modal editing system with two modes:

### Normal Mode

Default mode. Keys navigate, delete, yank, and send messages. Double-stroke commands (`dd`, `yy`, `gg`) use a `pending_command` state to detect the second keystroke.

### Insert Mode

Text entry mode. Keys are passed to the input `TextArea`. `Esc` returns to Normal mode. `Enter` sends the message. `:` triggers the emoji picker.

The mode is displayed in the input box's border title (cyan for Normal, green for Insert).
