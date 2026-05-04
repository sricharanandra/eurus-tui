# Eurus TUI

Eurus is an end-to-end encrypted terminal chat application with real-time voice chat. This repository contains the Rust TUI (Terminal User Interface) client.

## Quick Start

```bash
# Build (debug)
cargo build

# Run
cargo run

# Build for production
cargo build --release

# Run release binary
./target/release/eurus
```

## Architecture

Eurus TUI is a terminal application built with `ratatui` and `crossterm`. It connects to the Eurus server over WebSocket for real-time messaging and voice chat. All messages are encrypted end-to-end with AES-256-GCM. Authentication uses SSH key challenge-response — no passwords.

The application is structured around a single `App` state container with screen-based navigation, a dedicated tokio task for voice management, and a modal editing system inspired by Vim.

## Project Structure

```
src/
├── main.rs      # Application entry: App state, screens, rendering, commands, WebSocket
├── api.rs       # Protocol types (client↔server message schemas with serde)
├── voice/
│   ├── mod.rs   # Module re-exports
│   ├── manager.rs # Voice chat orchestrator (join/leave/mute, signal routing)
│   └── audio.rs   # Low-level audio engine (cpal streams, Opus encode/decode, resampling)
├── ssh.rs       # SSH key discovery (agent + file) and challenge-response signing
├── crypto.rs    # AES-256-GCM encrypt/decrypt utilities
├── config.rs    # TOML configuration loading with defaults
├── clipboard.rs # System clipboard integration (text + image via arboard)
├── emoji.rs     # Emoji shortcode database (~250 entries) and lookup
└── vim.rs       # Vim mode state machine (Normal/Insert)
```

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](architecture.md) | App structure, screen flow, WebSocket layer, task architecture |
| [UI](ui.md) | Rendering layout, Vim mode, emoji picker, overlays, color scheme |
| [Voice](voice.md) | VoiceManager task, AudioEngine, cpal, audiopus, resampling |
| [Commands](commands.md) | Full command reference with aliases |
| [Encryption](encryption.md) | AES-256-GCM e2e encryption, room keys, crypto.rs |

## Configuration

Configuration is loaded from `~/.config/eurus/config.toml`. If the file is missing or unparseable, defaults are used.

```toml
[server]
url = "wss://eurus.sreus.tech/ws"

[auth]
token_file = "~/.config/eurus/token"

[ui]
show_timestamps = true
message_limit = 1000
multiline_mode = false

[network]
reconnect_attempts = 10
ping_interval = 30
```

The server URL can also be overridden via the `EURUS_SERVER_URL` environment variable.

## Tech Stack

- **Language:** Rust 2021 edition
- **TUI:** `ratatui` 0.29 + `crossterm` 0.27
- **Async:** `tokio` (multi-threaded runtime)
- **WebSocket:** `tokio-tungstenite` 0.24 with rustls TLS
- **Voice:** `cpal` 0.15 (cross-platform audio), `audiopus` 0.2 (Opus codec)
- **Crypto:** `aes-gcm` 0.10, `ed25519-dalek` 2.1, `rsa` 0.9, `ssh-key` 0.6
- **Serialization:** `serde` + `serde_json`
- **Clipboard:** `arboard` 3.6
- **Notifications:** `notify-rust` 4 (desktop notifications)
