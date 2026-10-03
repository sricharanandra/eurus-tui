use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use base64::{Engine as _, engine::general_purpose};
use anyhow::{anyhow, Result};

use crate::voice::types::{
    AudioPayload, ClientMessage, JoinVoicePayload, LeaveVoicePayload,
};
use crate::voice::audio::{AudioEngine, AudioDeviceError};
use crate::voice::debug::voice_debug;

const TX_THROTTLE_MS: u64 = 200;
const TX_QUIET_TIMEOUT_MS: u64 = 600;
/// How long to wait for the server's voice_joined before giving up.
const JOIN_TIMEOUT: Duration = Duration::from_secs(5);
/// A mic error sooner than this after capture (re)started ends the session
/// instead of restarting capture again.
const CAPTURE_RESTART_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// Encoded mic packets waiting for the socket; older ones are dropped.
const ENCODED_QUEUE_LEN: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum VoiceConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

impl Default for VoiceConnectionStatus {
    fn default() -> Self {
        VoiceConnectionStatus::Disconnected
    }
}

#[derive(Debug)]
pub enum VoiceEvent {
    Connecting,
    Connected,
    /// Transport dropped mid-call; voice resumes when the room is rejoined.
    Disconnected,
    ConnectionFailed(String),
    VoiceJoined,
    VoiceLeft,
    VoiceStateChanged(Vec<String>),
    MuteStateChanged(bool),
    TxActivity(bool),
    AudioError(String),
    PeerJoined(String),
    PeerLeft(String),
}

pub enum VoiceCommand {
    Join { room_id: String },
    Leave,
    Mute(bool),
    SetWsSender(mpsc::UnboundedSender<String>),
    ServerAudio(crate::api::ServerAudioPayload),
    /// Server admitted this connection to voice in room_id.
    ServerVoiceJoined { room_id: String },
    /// Server refused or failed a voice join for room_id.
    ServerVoiceError { room_id: String, message: String },
    ConnectionStatusChanged(bool),
    /// Server confirmed the chat room this client is now in.
    RoomJoined { room_id: String },
}

/// Where the voice session stands. One value instead of several flags, so
/// combinations like "not in voice but will rejoin some old room" can't drift
/// out of sync.
#[derive(Debug)]
enum Session {
    Idle,
    /// join_voice sent; capture starts once the server confirms.
    Joining { room_id: String, deadline: tokio::time::Instant },
    /// Admitted and capturing. mic_task sends to the socket that was live
    /// when capture started, so it can't leak into a later connection.
    Live { room_id: String, mic_task: JoinHandle<()>, capture_started: Instant },
    /// Transport dropped mid-call; rejoin when this room is confirmed again.
    Suspended { room_id: String },
}

/// Owns the voice session. Runs as one task fed by VoiceCommand; all state
/// changes are reported to the UI as VoiceEvent.
pub struct VoiceManager {
    event_tx: mpsc::UnboundedSender<VoiceEvent>,
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    connected: bool,
    /// Shared with the mic task. Survives transport recovery; cleared on explicit leave.
    is_muted: Arc<AtomicBool>,
    audio: AudioEngine,
    audio_error_rx: Option<mpsc::UnboundedReceiver<AudioDeviceError>>,
    session: Session,
}

impl VoiceManager {
    pub fn new(event_tx: mpsc::UnboundedSender<VoiceEvent>) -> Self {
        let (audio_error_tx, audio_error_rx) = mpsc::unbounded_channel::<AudioDeviceError>();
        let mut audio = AudioEngine::new();
        audio.set_error_channel(audio_error_tx);

        Self {
            event_tx,
            ws_tx: None,
            connected: false,
            is_muted: Arc::new(AtomicBool::new(false)),
            audio,
            audio_error_rx: Some(audio_error_rx),
            session: Session::Idle,
        }
    }

    pub async fn run(
        &mut self,
        mut command_rx: mpsc::UnboundedReceiver<VoiceCommand>,
    ) {
        let mut audio_error_rx = self.audio_error_rx.take();

        loop {
            let join_deadline = match &self.session {
                Session::Joining { deadline, .. } => Some(*deadline),
                _ => None,
            };

            tokio::select! {
                cmd = command_rx.recv() => match cmd {
                    Some(cmd) => self.handle_command(cmd),
                    None => break,
                },
                Some(err) = async {
                    match audio_error_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_device_error(err);
                }
                _ = tokio::time::sleep_until(join_deadline.unwrap_or_else(tokio::time::Instant::now)),
                    if join_deadline.is_some() => {
                    self.handle_join_timeout();
                }
            }
        }

        self.end_session();
    }

    fn handle_command(&mut self, cmd: VoiceCommand) {
        match cmd {
            VoiceCommand::Join { room_id } => self.handle_join(room_id),
            VoiceCommand::Leave => self.handle_leave(),
            VoiceCommand::Mute(muted) => {
                self.is_muted.store(muted, Ordering::Relaxed);
                if muted {
                    self.emit(VoiceEvent::TxActivity(false));
                }
                self.emit(VoiceEvent::MuteStateChanged(muted));
            }
            VoiceCommand::SetWsSender(sender) => self.ws_tx = Some(sender),
            VoiceCommand::ServerAudio(payload) => self.handle_server_audio(payload),
            VoiceCommand::ServerVoiceJoined { room_id } => self.handle_voice_joined(room_id),
            VoiceCommand::ServerVoiceError { room_id, message } => self.handle_voice_error(room_id, message),
            VoiceCommand::ConnectionStatusChanged(connected) => self.handle_connection_status(connected),
            VoiceCommand::RoomJoined { room_id } => self.handle_room_joined(room_id),
        }
    }

    fn handle_join(&mut self, room_id: String) {
        match &self.session {
            Session::Live { room_id: current, .. } if *current == room_id => {
                self.emit(VoiceEvent::VoiceJoined);
                self.emit(VoiceEvent::Connected);
                return;
            }
            Session::Joining { room_id: current, .. } if *current == room_id => {
                self.emit(VoiceEvent::Connecting);
                return;
            }
            Session::Suspended { room_id: current } if *current == room_id && !self.connected => {
                self.emit(VoiceEvent::Disconnected);
                return;
            }
            _ => {}
        }

        // Anything else belongs to another room or a dead transport.
        self.end_session();
        self.request_join(room_id);
    }

    /// Ask the server for voice in room_id. Capture waits for voice_joined.
    fn request_join(&mut self, room_id: String) {
        if !self.connected {
            self.emit(VoiceEvent::ConnectionFailed("Not connected to server".to_string()));
            return;
        }
        voice_debug!("join voice room_id={}", room_id);
        let msg = ClientMessage::JoinVoice {
            payload: JoinVoicePayload { room_id: room_id.clone() },
        };
        if let Err(e) = self.send(&msg) {
            self.emit(VoiceEvent::ConnectionFailed(e.to_string()));
            return;
        }
        self.session = Session::Joining {
            room_id,
            deadline: tokio::time::Instant::now() + JOIN_TIMEOUT,
        };
        self.emit(VoiceEvent::Connecting);
    }

    fn handle_voice_joined(&mut self, room_id: String) {
        // Ignore confirmations for joins that were since cancelled or replaced.
        match &self.session {
            Session::Joining { room_id: pending, .. } if *pending == room_id => {}
            _ => return,
        }

        match self.start_capture(&room_id) {
            Ok(mic_task) => {
                self.session = Session::Live { room_id, mic_task, capture_started: Instant::now() };
                self.emit(VoiceEvent::VoiceJoined);
                self.emit(VoiceEvent::Connected);
            }
            Err(e) => {
                voice_debug!("start_capture failed: {}", e);
                // Back out the server-side session so we don't leave a ghost.
                let _ = self.send(&ClientMessage::LeaveVoice {
                    payload: LeaveVoicePayload { room_id },
                });
                self.session = Session::Idle;
                self.emit(VoiceEvent::AudioError(format!("Mic error: {}", e)));
                self.emit(VoiceEvent::ConnectionFailed("Failed to start microphone".to_string()));
            }
        }
    }

    fn handle_voice_error(&mut self, room_id: String, message: String) {
        if matches!(&self.session, Session::Joining { room_id: pending, .. } if *pending == room_id) {
            voice_debug!("voice join rejected room_id={}: {}", room_id, message);
            self.session = Session::Idle;
            self.emit(VoiceEvent::ConnectionFailed(message));
        }
    }

    fn handle_join_timeout(&mut self) {
        if let Session::Joining { room_id, .. } = std::mem::replace(&mut self.session, Session::Idle) {
            voice_debug!("voice join timed out room_id={}", room_id);
            // In case the server admits us after all.
            let _ = self.send(&ClientMessage::LeaveVoice {
                payload: LeaveVoicePayload { room_id },
            });
            self.emit(VoiceEvent::ConnectionFailed("Server did not confirm voice join".to_string()));
        }
    }

    /// Explicit leave: ends any session and cancels pending rejoin, even
    /// while disconnected. A no-op when idle so callers can send it freely.
    fn handle_leave(&mut self) {
        if matches!(self.session, Session::Idle) {
            return;
        }
        voice_debug!("leave voice");
        self.end_session();
        self.is_muted.store(false, Ordering::Relaxed);
        self.emit(VoiceEvent::TxActivity(false));
        self.emit(VoiceEvent::MuteStateChanged(false));
        self.emit(VoiceEvent::VoiceLeft);
    }

    /// Stop capture/playback and tell the server, without emitting UI events.
    fn end_session(&mut self) {
        match std::mem::replace(&mut self.session, Session::Idle) {
            Session::Live { room_id, mic_task, .. } => {
                mic_task.abort();
                self.audio.reset();
                let _ = self.send(&ClientMessage::LeaveVoice {
                    payload: LeaveVoicePayload { room_id },
                });
            }
            Session::Joining { room_id, .. } => {
                let _ = self.send(&ClientMessage::LeaveVoice {
                    payload: LeaveVoicePayload { room_id },
                });
            }
            Session::Suspended { .. } | Session::Idle => {}
        }
    }

    fn handle_connection_status(&mut self, connected: bool) {
        self.connected = connected;
        if connected {
            // Voice rejoin happens on RoomJoined, once the chat room is back.
            voice_debug!("ws connected");
            return;
        }

        voice_debug!("ws disconnected");
        let room_id = match std::mem::replace(&mut self.session, Session::Idle) {
            Session::Live { room_id, mic_task, .. } => {
                mic_task.abort();
                self.audio.reset();
                room_id
            }
            Session::Joining { room_id, .. } | Session::Suspended { room_id } => room_id,
            Session::Idle => return,
        };
        voice_debug!("suspended voice for reconnect room_id={}", room_id);
        self.session = Session::Suspended { room_id };
        self.emit(VoiceEvent::TxActivity(false));
        self.emit(VoiceEvent::Disconnected);
    }

    /// Reconcile voice with the confirmed chat room: resume a suspended call
    /// in the same room, or end voice that belongs to a room we've left.
    fn handle_room_joined(&mut self, room_id: String) {
        match &self.session {
            Session::Idle => {}
            Session::Suspended { room_id: r } if *r == room_id => {
                voice_debug!("auto-rejoining voice after reconnect room_id={}", room_id);
                self.session = Session::Idle;
                self.request_join(room_id);
            }
            Session::Live { room_id: r, .. } | Session::Joining { room_id: r, .. } if *r == room_id => {}
            _ => self.handle_leave(),
        }
    }

    /// Start mic capture and the task that forwards encoded audio to the socket.
    fn start_capture(&mut self, room_id: &str) -> Result<JoinHandle<()>> {
        let ws = self.ws_tx.clone().ok_or_else(|| anyhow!("Not connected to server"))?;
        let (encoded_tx, mut encoded_rx) = mpsc::channel::<(Vec<u8>, bool)>(ENCODED_QUEUE_LEN);
        self.audio.start_capture(encoded_tx)?;

        let event_tx = self.event_tx.clone();
        let is_muted = self.is_muted.clone();
        let room_id = room_id.to_string();

        Ok(tokio::spawn(async move {
            // Per-task counters: a new session never continues an old one's sequence.
            let mut seq: u32 = 0;
            let mut last_tx_emit: Option<Instant> = None;
            let mut last_active = false;
            let mut quiet_since: Option<Instant> = None;

            voice_debug!("mic task started room_id={}", room_id);

            while let Some((packet, active)) = encoded_rx.recv().await {
                if is_muted.load(Ordering::Relaxed) {
                    continue;
                }

                let msg = ClientMessage::Audio {
                    payload: AudioPayload {
                        room_id: room_id.clone(),
                        seq,
                        timestamp: seq as u64 * 20,
                        data: general_purpose::STANDARD.encode(&packet),
                    },
                };
                seq = seq.wrapping_add(1);
                if let Ok(json) = serde_json::to_string(&msg) {
                    if ws.send(json).is_err() {
                        voice_debug!("mic task: ws send failed, exiting");
                        break;
                    }
                }

                // TxActivity with throttle and quiet-timeout
                let now = Instant::now();
                if active {
                    let should_emit = last_tx_emit
                        .map(|t| now.duration_since(t) > Duration::from_millis(TX_THROTTLE_MS))
                        .unwrap_or(true);
                    if should_emit || !last_active {
                        let _ = event_tx.send(VoiceEvent::TxActivity(true));
                        last_tx_emit = Some(now);
                        last_active = true;
                        quiet_since = None;
                    }
                } else if last_active {
                    let since = *quiet_since.get_or_insert(now);
                    if now.duration_since(since) > Duration::from_millis(TX_QUIET_TIMEOUT_MS) {
                        let _ = event_tx.send(VoiceEvent::TxActivity(false));
                        last_active = false;
                        last_tx_emit = None;
                    }
                }
            }

            voice_debug!("mic task ended room_id={}", room_id);
            let _ = event_tx.send(VoiceEvent::TxActivity(false));
        }))
    }

    fn handle_server_audio(&mut self, payload: crate::api::ServerAudioPayload) {
        // Only play audio for the live session's room. Covers in-flight
        // packets after leaving, switching rooms, or reconnecting.
        match &self.session {
            Session::Live { room_id, .. } if *room_id == payload.room_id => {}
            _ => return,
        }

        let audio_data = match general_purpose::STANDARD.decode(&payload.payload) {
            Ok(d) => d,
            Err(e) => {
                voice_debug!("audio decode error from {}: {}", payload.user_id, e);
                self.emit(VoiceEvent::AudioError(format!("Decode error: {}", e)));
                return;
            }
        };

        if let Err(e) = self.audio.push_peer_packet(&payload.user_id, audio_data) {
            voice_debug!("playback error for {}: {}", payload.user_id, e);
            self.emit(VoiceEvent::AudioError(format!("Playback error: {}", e)));
        }
    }

    fn handle_device_error(&mut self, err: AudioDeviceError) {
        match err {
            AudioDeviceError::OutputDeviceError(e) => {
                voice_debug!("output device error: {}", e);
                // Streams rebuild on the next packet, retrying a missing device.
                self.audio.drop_playback();
                self.emit(VoiceEvent::AudioError(format!("Output error: {}", e)));
            }
            AudioDeviceError::InputDeviceError(e) => {
                voice_debug!("input device error: {}", e);
                self.emit(VoiceEvent::AudioError(format!("Input error: {}", e)));
                let room_id = match &self.session {
                    Session::Live { room_id, capture_started, .. }
                        if capture_started.elapsed() >= CAPTURE_RESTART_MIN_INTERVAL => room_id.clone(),
                    Session::Live { .. } => return self.fail_session("Microphone keeps failing"),
                    _ => return,
                };
                match self.start_capture(&room_id) {
                    Ok(mic_task) => {
                        let old = std::mem::replace(
                            &mut self.session,
                            Session::Live { room_id, mic_task, capture_started: Instant::now() },
                        );
                        if let Session::Live { mic_task, .. } = old {
                            mic_task.abort();
                        }
                    }
                    Err(e) => self.fail_session(&format!("Microphone failed: {}", e)),
                }
            }
        }
    }

    /// End a session that can't continue and say why.
    fn fail_session(&mut self, reason: &str) {
        self.handle_leave();
        self.emit(VoiceEvent::ConnectionFailed(reason.to_string()));
    }

    fn send(&self, msg: &ClientMessage) -> Result<()> {
        let ws = self.ws_tx.as_ref().ok_or_else(|| anyhow!("Not connected to server"))?;
        let json = serde_json::to_string(msg)?;
        ws.send(json).map_err(|_| anyhow!("Connection closed"))
    }

    fn emit(&self, event: VoiceEvent) {
        let _ = self.event_tx.send(event);
    }
}
