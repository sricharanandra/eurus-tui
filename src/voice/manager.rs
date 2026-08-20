use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use tokio::time::Duration;
use base64::{Engine as _, engine::general_purpose};
use anyhow::Result;

use crate::voice::types::{
    AudioPayload, ClientMessage, LeaveVoicePayload,
};
use crate::voice::audio::{AudioEngine, AudioDeviceError};
use crate::voice::debug::voice_debug;

const TX_THROTTLE_MS: u64 = 200;
const TX_QUIET_TIMEOUT_MS: u64 = 600;

#[derive(Debug, Clone, PartialEq)]
pub enum VoiceState {
    Disconnected,
    Connected,
    InRoom,
    InVoice,
    Streaming,
}

impl Default for VoiceState {
    fn default() -> Self {
        VoiceState::Disconnected
    }
}

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
    ConnectionStatusChanged(bool),
    RoomJoined,
}

pub struct VoiceManager {
    state: Arc<Mutex<VoiceState>>,
    status: Arc<Mutex<VoiceConnectionStatus>>,
    room_id: Arc<Mutex<Option<String>>>,
    event_tx: mpsc::UnboundedSender<VoiceEvent>,
    is_muted: Arc<AtomicBool>,
    seq: Arc<AtomicU32>,
    timestamp: Arc<AtomicU64>,
    audio_engine: Arc<Mutex<AudioEngine>>,
    audio_error_rx: Option<mpsc::UnboundedReceiver<AudioDeviceError>>,
    ws_tx: Arc<StdMutex<Option<mpsc::UnboundedSender<String>>>>,
    in_voice: Arc<AtomicBool>,
    was_in_voice: Arc<AtomicBool>,
}

impl VoiceManager {
    pub fn new(event_tx: mpsc::UnboundedSender<VoiceEvent>) -> Self {
        let (audio_error_tx, audio_error_rx) = mpsc::unbounded_channel::<AudioDeviceError>();
        let mut audio_engine = AudioEngine::new();
        let _ = audio_engine.set_error_channel(audio_error_tx);

        Self {
            state: Arc::new(Mutex::new(VoiceState::Disconnected)),
            status: Arc::new(Mutex::new(VoiceConnectionStatus::Disconnected)),
            room_id: Arc::new(Mutex::new(None)),
            event_tx,
            is_muted: Arc::new(AtomicBool::new(false)),
            seq: Arc::new(AtomicU32::new(0)),
            timestamp: Arc::new(AtomicU64::new(0)),
            audio_engine: Arc::new(Mutex::new(audio_engine)),
            audio_error_rx: Some(audio_error_rx),
            ws_tx: Arc::new(StdMutex::new(None)),
            in_voice: Arc::new(AtomicBool::new(false)),
            was_in_voice: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_ws_sender(&self, tx: mpsc::UnboundedSender<String>) {
        let mut ws = self.ws_tx.lock().unwrap();
        *ws = Some(tx);
    }

    pub async fn run(
        &mut self,
        mut command_rx: mpsc::UnboundedReceiver<VoiceCommand>,
    ) {
        let mut audio_error_rx = self.audio_error_rx.take();

        loop {
            tokio::select! {
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        VoiceCommand::Join { room_id } => {
                            self.handle_join(room_id).await;
                        }
                        VoiceCommand::Leave => {
                            self.handle_leave().await;
                        }
                        VoiceCommand::Mute(muted) => {
                            self.is_muted.store(muted, Ordering::Relaxed);
                            let _ = self.event_tx.send(VoiceEvent::MuteStateChanged(muted));
                        }
                        VoiceCommand::SetWsSender(sender) => {
                            self.set_ws_sender(sender);
                        }
                        VoiceCommand::ServerAudio(payload) => {
                            self.handle_server_audio(payload).await;
                        }
                        VoiceCommand::ConnectionStatusChanged(connected) => {
                            self.handle_connection_status(connected).await;
                        }
                        VoiceCommand::RoomJoined => {
                            self.handle_room_joined().await;
                        }
                    }
                }
                Some(err) = async {
                    if let Some(ref mut rx) = audio_error_rx {
                        rx.recv().await
                    } else {
                        None
                    }
                } => {
                    let err_msg = match err {
                        AudioDeviceError::OutputDeviceError(e) => format!("Output error: {}", e),
                        AudioDeviceError::InputDeviceError(e) => format!("Input error: {}", e),
                    };
                    voice_debug!("audio device error: {}", err_msg);
                    let _ = self.event_tx.send(VoiceEvent::AudioError(err_msg));
                }
                else => break,
            }
        }
    }

    async fn handle_join(&self, room_id: String) {
        if self.in_voice.load(Ordering::Relaxed) {
            let _ = self.event_tx.send(VoiceEvent::ConnectionFailed(
                "Already in voice".to_string(),
            ));
            return;
        }

        {
            let state = self.state.lock().await;
            if *state != VoiceState::Connected && *state != VoiceState::InRoom {
                let _ = self.event_tx.send(VoiceEvent::ConnectionFailed(
                    "Not connected to server".to_string(),
                ));
                return;
            }
        }

        voice_debug!("join voice room_id={}", room_id);
        let _ = self.event_tx.send(VoiceEvent::Connecting);

        if let Err(e) = self.start_voice_session(&room_id).await {
            voice_debug!("start_voice_session failed: {}", e);
            let _ = self.event_tx.send(VoiceEvent::AudioError(format!("Mic error: {}", e)));
            let _ = self.event_tx.send(VoiceEvent::ConnectionFailed("Failed to start microphone".to_string()));
            return;
        }

        let _ = self.event_tx.send(VoiceEvent::VoiceJoined);
        let _ = self.event_tx.send(VoiceEvent::Connected);
    }

    async fn handle_leave(&self) {
        if !self.in_voice.load(Ordering::Relaxed) {
            let _ = self.event_tx.send(VoiceEvent::VoiceLeft);
            return;
        }

        voice_debug!("leave voice");
        self.stop_voice_session().await;

        let room_id = {
            let mut room = self.room_id.lock().await;
            room.take()
        };

        if let Some(rid) = room_id {
            let msg = ClientMessage::LeaveVoice {
                payload: LeaveVoicePayload { room_id: rid },
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                self.send_ws(&json);
            }
        }

        self.reset_voice_state().await;
        let _ = self.event_tx.send(VoiceEvent::VoiceLeft);
    }

    async fn handle_connection_status(&self, connected: bool) {
        if connected {
            voice_debug!("ws connected");
            let mut state = self.state.lock().await;
            if *state == VoiceState::Disconnected {
                *state = VoiceState::Connected;
            }
            // Voice rejoin happens on RoomJoined, not here, to ensure the
            // chat room is re-joined before we send join_voice.
        } else {
            voice_debug!("ws disconnected");
            let was_in_voice = self.in_voice.load(Ordering::Relaxed);
            {
                let mut audio = self.audio_engine.lock().await;
                audio.reset();
            }
            self.in_voice.store(false, Ordering::Relaxed);
            if was_in_voice {
                self.was_in_voice.store(true, Ordering::Relaxed);
                voice_debug!("marked was_in_voice for reconnect rejoin");
            }
            {
                let mut state = self.state.lock().await;
                *state = VoiceState::Disconnected;
            }
            self.reset_voice_state_internal().await;
            let _ = self.event_tx.send(VoiceEvent::Disconnected);
        }
    }

    async fn handle_room_joined(&self) {
        let should_rejoin = self.was_in_voice.load(Ordering::Relaxed)
            && !self.in_voice.load(Ordering::Relaxed);

        {
            let mut state = self.state.lock().await;
            if *state == VoiceState::Connected || *state == VoiceState::Disconnected {
                *state = VoiceState::InRoom;
            }
        }

        if should_rejoin {
            let room_id = {
                let room = self.room_id.lock().await;
                room.clone()
            };
            if let Some(rid) = room_id {
                voice_debug!("auto-rejoining voice after reconnect room_id={}", rid);
                self.was_in_voice.store(false, Ordering::Relaxed);
                if let Err(e) = self.start_voice_session(&rid).await {
                    voice_debug!("auto-rejoin failed: {}", e);
                    let _ = self.event_tx.send(VoiceEvent::AudioError(format!("Mic error: {}", e)));
                } else {
                    let _ = self.event_tx.send(VoiceEvent::VoiceJoined);
                    let _ = self.event_tx.send(VoiceEvent::Connected);
                }
            } else {
                self.was_in_voice.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Send join_voice, then start mic capture + spawn the mic→ws sender task.
    /// Caller is responsible for guarding against double-join.
    async fn start_voice_session(&self, room_id: &str) -> Result<()> {
        self.seq.store(0, Ordering::Relaxed);
        self.timestamp.store(0, Ordering::Relaxed);

        {
            let mut room = self.room_id.lock().await;
            *room = Some(room_id.to_string());
        }

        // Join on the server FIRST so early mic packets aren't dropped as strays.
        let msg = ClientMessage::JoinVoice {
            payload: crate::voice::types::JoinVoicePayload {
                room_id: room_id.to_string(),
            },
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            self.send_ws(&json);
        }

        let (encoded_tx, mut encoded_rx) = mpsc::unbounded_channel::<(Vec<u8>, bool)>();
        {
            let mut audio = self.audio_engine.lock().await;
            audio.start_capture(encoded_tx)?;
        }

        self.in_voice.store(true, Ordering::Relaxed);
        self.was_in_voice.store(true, Ordering::Relaxed);

        let ws_tx = self.ws_tx.clone();
        let event_tx = self.event_tx.clone();
        let seq = self.seq.clone();
        let timestamp = self.timestamp.clone();
        let is_muted = self.is_muted.clone();
        let room_id_owned = room_id.to_string();

        tokio::spawn(async move {
            let mut last_tx_emit: Option<Instant> = None;
            let mut last_active = false;
            let mut quiet_since: Option<Instant> = None;

            voice_debug!("mic task started room_id={}", room_id_owned);

            while let Some((packet, active)) = encoded_rx.recv().await {
                if is_muted.load(Ordering::Relaxed) {
                    continue;
                }

                let ws_opt = ws_tx.lock().unwrap().clone();
                if let Some(ws) = ws_opt {
                    let current_seq = seq.fetch_add(1, Ordering::Relaxed);
                    let current_ts = timestamp.fetch_add(20, Ordering::Relaxed);

                    let payload = AudioPayload {
                        room_id: room_id_owned.clone(),
                        seq: current_seq,
                        timestamp: current_ts,
                        data: general_purpose::STANDARD.encode(&packet),
                    };

                    let msg = ClientMessage::Audio { payload };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        if ws.send(json).is_err() {
                            voice_debug!("mic task: ws send failed, exiting");
                            break;
                        }
                    }
                } else {
                    voice_debug!("mic task: no ws sender, dropping packet");
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
                    if quiet_since.is_none() {
                        quiet_since = Some(now);
                    }
                    if let Some(q) = quiet_since {
                        if now.duration_since(q) > Duration::from_millis(TX_QUIET_TIMEOUT_MS) {
                            let _ = event_tx.send(VoiceEvent::TxActivity(false));
                            last_active = false;
                            last_tx_emit = None;
                        }
                    }
                }
            }

            voice_debug!("mic task ended room_id={}", room_id_owned);
            let _ = event_tx.send(VoiceEvent::TxActivity(false));
        });

        {
            let mut state = self.state.lock().await;
            *state = VoiceState::Streaming;
        }

        Ok(())
    }

    /// Stop mic capture, drop all audio streams, clear in_voice flag.
    async fn stop_voice_session(&self) {
        {
            let mut audio = self.audio_engine.lock().await;
            audio.reset();
        }
        self.in_voice.store(false, Ordering::Relaxed);
        self.was_in_voice.store(false, Ordering::Relaxed);
    }

    async fn handle_server_audio(&self, payload: crate::api::ServerAudioPayload) {
        // Drop packets when not in voice - covers in-flight packets after
        // leaving and server-side races during reconnect.
        if !self.in_voice.load(Ordering::Relaxed) {
            return;
        }

        let audio_data = match general_purpose::STANDARD.decode(&payload.payload) {
            Ok(d) => d,
            Err(e) => {
                voice_debug!("audio decode error from {}: {}", payload.user_id, e);
                let _ = self.event_tx.send(VoiceEvent::AudioError(format!("Decode error: {}", e)));
                return;
            }
        };

        let mut engine = self.audio_engine.lock().await;
        if let Err(e) = engine.push_peer_packet(&payload.user_id, audio_data) {
            voice_debug!("playback error for {}: {}", payload.user_id, e);
            let _ = self.event_tx.send(VoiceEvent::AudioError(format!("Playback error: {}", e)));
        }
    }

    async fn reset_voice_state_internal(&self) {
        self.seq.store(0, Ordering::Relaxed);
        self.timestamp.store(0, Ordering::Relaxed);
        let _ = self.event_tx.send(VoiceEvent::TxActivity(false));
        let _ = self.event_tx.send(VoiceEvent::MuteStateChanged(false));
    }

    async fn reset_voice_state(&self) {
        {
            let mut state = self.state.lock().await;
            *state = VoiceState::InRoom;
        }
        self.reset_voice_state_internal().await;
    }

    fn send_ws(&self, json: &str) {
        let ws_opt = self.ws_tx.lock().unwrap().clone();
        if let Some(ws) = ws_opt {
            let _ = ws.send(json.to_string());
        }
    }

    pub async fn get_state(&self) -> VoiceState {
        self.state.lock().await.clone()
    }

    pub async fn get_status(&self) -> VoiceConnectionStatus {
        self.status.lock().await.clone()
    }

    pub async fn set_status(&self, status: VoiceConnectionStatus) {
        let mut current = self.status.lock().await;
        *current = status;
    }
}
