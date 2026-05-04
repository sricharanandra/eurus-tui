use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration};
use base64::{Engine as _, engine::general_purpose};
use anyhow::Result;

use crate::voice::types::{
    AudioPayload, ClientMessage, LeaveVoicePayload, ServerMessage,
};
use crate::voice::audio::{AudioEngine, AudioDeviceError};

const MAX_SEND_QUEUE: usize = 3;
const RECONNECT_BASE_DELAY_MS: u64 = 1000;
const MAX_RECONNECT_DELAY_MS: u64 = 30000;

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
    AudioData(Vec<u8>),
}

pub struct VoiceManager {
    state: Arc<Mutex<VoiceState>>,
    status: Arc<Mutex<VoiceConnectionStatus>>,
    room_id: Arc<Mutex<Option<String>>>,
    event_tx: mpsc::UnboundedSender<VoiceEvent>,
    is_muted: Arc<AtomicBool>,
    seq: Arc<AtomicU32>,
    timestamp: Arc<AtomicU64>,
    send_queue: Arc<Mutex<Vec<String>>>,
    reconnect_delay: Arc<Mutex<u64>>,
    audio_engine: Arc<Mutex<AudioEngine>>,
    audio_error_rx: Option<mpsc::UnboundedReceiver<AudioDeviceError>>,
    ws_tx: Arc<StdMutex<Option<mpsc::UnboundedSender<String>>>>,
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
            send_queue: Arc::new(Mutex::new(Vec::with_capacity(MAX_SEND_QUEUE))),
            reconnect_delay: Arc::new(Mutex::new(RECONNECT_BASE_DELAY_MS)),
            audio_engine: Arc::new(Mutex::new(audio_engine)),
            audio_error_rx: Some(audio_error_rx),
            ws_tx: Arc::new(StdMutex::new(None)),
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
                        VoiceCommand::AudioData(data) => {
                            self.handle_audio_data(data).await;
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
                    let _ = self.event_tx.send(VoiceEvent::AudioError(err_msg));
                }
                else => break,
            }
        }
    }

    async fn handle_join(&self, room_id: String) {
        {
            let mut state = self.state.lock().await;
            if *state != VoiceState::Connected && *state != VoiceState::InRoom {
                let _ = self.event_tx.send(VoiceEvent::ConnectionFailed(
                    "Not connected to server".to_string(),
                ));
                return;
            }
        }

        let _ = self.event_tx.send(VoiceEvent::Connecting);

        {
            let mut state = self.state.lock().await;
            *state = VoiceState::InVoice;
        }
        {
            let mut room = self.room_id.lock().await;
            *room = Some(room_id.clone());
        }

        let (encoded_tx, mut encoded_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let ws_tx_sender = self.ws_tx.lock().unwrap().clone();
        let seq = self.seq.clone();
        let timestamp = self.timestamp.clone();
        let is_muted = self.is_muted.clone();
        let send_queue = self.send_queue.clone();
        let room_id_for_spawn = room_id.clone();

        tokio::spawn(async move {
            while let Some(packet) = encoded_rx.recv().await {
                if is_muted.load(Ordering::Relaxed) {
                    continue;
                }

                let current_seq = seq.fetch_add(1, Ordering::Relaxed);
                let current_ts = timestamp.fetch_add(20, Ordering::Relaxed);

                let payload = AudioPayload {
                    room_id: room_id_for_spawn.clone(),
                    seq: current_seq,
                    timestamp: current_ts,
                    data: general_purpose::STANDARD.encode(&packet),
                };

                let msg = ClientMessage::Audio { payload };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let mut queue = send_queue.lock().await;
                    if queue.len() >= MAX_SEND_QUEUE {
                        queue.remove(0);
                    }
                    queue.push(json);
                    drop(queue);

                    if let Some(ws) = ws_tx_sender.as_ref() {
                        if let Some(msg) = send_queue.lock().await.pop() {
                            let _ = ws.send(msg);
                        }
                    }
                }
            }
        });

        {
            let mut audio = self.audio_engine.lock().await;
            if let Err(e) = audio.start_capture(encoded_tx) {
                let _ = self.event_tx.send(VoiceEvent::AudioError(format!("Mic error: {}", e)));
                let _ = self.event_tx.send(VoiceEvent::ConnectionFailed("Failed to start microphone".to_string()));
                return;
            }
        }

        let msg = ClientMessage::JoinVoice {
            payload: crate::voice::types::JoinVoicePayload {
                room_id: room_id.clone(),
            },
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            if let Some(ws) = self.ws_tx.lock().unwrap().as_ref() {
                let _ = ws.send(json);
            }
        }

        {
            let mut state = self.state.lock().await;
            *state = VoiceState::Streaming;
        }

        let _ = self.event_tx.send(VoiceEvent::VoiceJoined);
        let _ = self.event_tx.send(VoiceEvent::Connected);
    }

    async fn handle_leave(&self) {
        {
            let mut audio = self.audio_engine.lock().await;
            audio.reset();
        }

        let room_id = {
            let mut room = self.room_id.lock().await;
            room.take()
        };

        if let Some(rid) = room_id {
            let msg = ClientMessage::LeaveVoice {
                payload: LeaveVoicePayload { room_id: rid },
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                if let Some(ws) = self.ws_tx.lock().unwrap().as_ref() {
                    let _ = ws.send(json);
                }
            }
        }

        self.reset_voice_state().await;
        let _ = self.event_tx.send(VoiceEvent::VoiceLeft);
    }

    async fn handle_audio_data(&self, data: Vec<u8>) {
        if self.is_muted.load(Ordering::Relaxed) {
            return;
        }

        let room_id = {
            let room = self.room_id.lock().await;
            room.clone()
        };

        if room_id.is_none() {
            return;
        }

        let room_id = room_id.unwrap();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let timestamp = self.timestamp.fetch_add(20, Ordering::Relaxed);

        let payload = AudioPayload {
            room_id,
            seq,
            timestamp,
            data: general_purpose::STANDARD.encode(&data),
        };

        let msg = ClientMessage::Audio { payload };
        if let Ok(json) = serde_json::to_string(&msg) {
            let mut queue = self.send_queue.lock().await;
            if queue.len() >= MAX_SEND_QUEUE {
                queue.remove(0);
            }
            queue.push(json);
            drop(queue);

            let ws_opt = self.ws_tx.lock().unwrap().clone();
            if let Some(ws) = ws_opt {
                if let Some(msg) = self.send_queue.lock().await.pop() {
                    let _ = ws.send(msg);
                }
            }
        }

        let _ = self.event_tx.send(VoiceEvent::TxActivity(true));
    }

    async fn reset_voice_state(&self) {
        {
            let mut state = self.state.lock().await;
            *state = VoiceState::InRoom;
        }
        self.seq.store(0, Ordering::Relaxed);
        self.timestamp.store(0, Ordering::Relaxed);
        self.send_queue.lock().await.clear();
        let _ = self.event_tx.send(VoiceEvent::TxActivity(false));
        let _ = self.event_tx.send(VoiceEvent::MuteStateChanged(false));
    }

    pub async fn set_state(&self, state: VoiceState) {
        let mut current = self.state.lock().await;
        *current = state;
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

    pub fn parse_server_message(&self, json: &str) -> Option<VoiceEvent> {
        let msg: ServerMessage = serde_json::from_str(json).ok()?;

        match msg {
            ServerMessage::VoiceState { payload } => {
                Some(VoiceEvent::VoiceStateChanged(payload.active_users))
            }
            ServerMessage::VoiceJoined { .. } => Some(VoiceEvent::VoiceJoined),
            ServerMessage::VoiceLeft { .. } => Some(VoiceEvent::VoiceLeft),
            ServerMessage::Audio { payload } => {
                let audio_data = match general_purpose::STANDARD.decode(&payload.data) {
                    Ok(d) => d,
                    Err(e) => {
                        return Some(VoiceEvent::AudioError(format!("Decode error: {}", e)));
                    }
                };

                let audio = self.audio_engine.clone();
                let user_id = payload.user_id.clone();
                tokio::spawn(async move {
                    let (packet_tx, packet_rx) = mpsc::unbounded_channel();
                    let _ = packet_tx.send(audio_data);

                    let mut engine = audio.lock().await;
                    if let Err(e) = engine.start_playback_for_peer(&user_id, packet_rx) {
                        eprintln!("[VOICE] Playback error for {}: {}", user_id, e);
                    }
                });

                None
            }
        }
    }
}