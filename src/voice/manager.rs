use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, Mutex};
use anyhow::Result;

use crate::voice::audio::{AudioEngine, AudioDeviceError};

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
    Signal { target_id: Option<String>, signal_type: String, data: String },
    Connecting,
    Connected,
    Disconnected,
    ConnectionFailed(String),
    MuteStateChanged(bool),
    TxActivity(bool),
    AudioError(String),
}

pub enum VoiceCommand {
    Join(String),
    Leave,
    Mute(bool),
    Signal { sender_id: String, signal_type: String, data: String },
}

pub struct VoiceManager {
    room_id: Option<String>,
    event_tx: mpsc::UnboundedSender<VoiceEvent>,
    audio_engine: Arc<Mutex<AudioEngine>>,
    audio_error_rx: Option<mpsc::UnboundedReceiver<AudioDeviceError>>,
    playback_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    is_muted: Arc<AtomicBool>,
    is_joined: Arc<AtomicBool>,
}

impl VoiceManager {
    pub fn new(event_tx: mpsc::UnboundedSender<VoiceEvent>) -> Self {
        let (audio_error_tx, audio_error_rx) = mpsc::unbounded_channel::<AudioDeviceError>();

        let mut audio_engine = AudioEngine::new();
        audio_engine.set_error_channel(audio_error_tx);

        Self {
            room_id: None,
            event_tx,
            audio_engine: Arc::new(Mutex::new(audio_engine)),
            audio_error_rx: Some(audio_error_rx),
            playback_tx: None,
            is_muted: Arc::new(AtomicBool::new(false)),
            is_joined: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn run(&mut self, mut command_rx: mpsc::UnboundedReceiver<VoiceCommand>) {
        let mut audio_error_rx = self.audio_error_rx.take();

        loop {
            tokio::select! {
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        VoiceCommand::Join(room_id) => {
                            let _ = self.join_voice(room_id).await;
                        }
                        VoiceCommand::Leave => {
                            let _ = self.leave_voice().await;
                        }
                        VoiceCommand::Mute(muted) => {
                            self.is_muted.store(muted, Ordering::Relaxed);
                            let _ = self.event_tx.send(VoiceEvent::MuteStateChanged(muted));
                        }
                        VoiceCommand::Signal { sender_id, signal_type, data } => {
                            let _ = self.handle_signal(&sender_id, &signal_type, &data).await;
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
                        AudioDeviceError::OutputDeviceError(e) => format!("Output device error: {}", e),
                        AudioDeviceError::InputDeviceError(e) => format!("Input device error: {}", e),
                    };
                    let _ = self.event_tx.send(VoiceEvent::AudioError(err_msg));
                }
                else => break,
            }
        }
    }

    async fn join_voice(&mut self, room_id: String) -> Result<()> {
        let _ = self.event_tx.send(VoiceEvent::Connecting);

        {
            let mut audio = self.audio_engine.lock().await;
            audio.reset();
        }

        self.room_id = Some(room_id.clone());
        self.is_joined.store(true, Ordering::Relaxed);

        let (encoded_tx, mut encoded_rx) = mpsc::unbounded_channel();
        {
            let mut audio = self.audio_engine.lock().await;
            if let Err(e) = audio.start_capture(encoded_tx) {
                let err_msg = format!("Failed to start microphone: {}", e);
                self.is_joined.store(false, Ordering::Relaxed);
                self.room_id = None;
                let _ = self.event_tx.send(VoiceEvent::ConnectionFailed(err_msg));
                return Err(anyhow::anyhow!("Microphone capture failed"));
            }
        }

        let is_muted = self.is_muted.clone();
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some(packet) = encoded_rx.recv().await {
                if is_muted.load(Ordering::Relaxed) {
                    continue;
                }

                let _ = event_tx.send(VoiceEvent::TxActivity(true));

                let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &packet);
                let _ = event_tx.send(VoiceEvent::Signal {
                    target_id: Some("server".to_string()),
                    signal_type: "audio".to_string(),
                    data: encoded,
                });
            }
            let _ = event_tx.send(VoiceEvent::TxActivity(false));
        });

        // Set up playback channel for receiving mixed audio from server
        let (playback_tx, playback_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        {
            let mut audio = self.audio_engine.lock().await;
            if let Err(e) = audio.start_playback_for_peer("server", playback_rx) {
                let err_msg = format!("Audio playback failed: {}", e);
                let _ = self.event_tx.send(VoiceEvent::ConnectionFailed(err_msg));
                self.is_joined.store(false, Ordering::Relaxed);
                self.room_id = None;
                return Err(anyhow::anyhow!("Audio playback failed"));
            }
        }

        // Store playback_tx for receiving audio frames from handle_signal
        self.playback_tx = Some(playback_tx);

        // Send join_voice signal to server
        self.event_tx.send(VoiceEvent::Signal {
            target_id: Some("server".to_string()),
            signal_type: "join_voice".to_string(),
            data: "".to_string(),
        })?;

        let _ = self.event_tx.send(VoiceEvent::Connected);
        Ok(())
    }

    async fn leave_voice(&mut self) -> Result<()> {
        self.is_joined.store(false, Ordering::Relaxed);

        if self.room_id.is_some() {
            let _ = self.event_tx.send(VoiceEvent::Signal {
                target_id: Some("server".to_string()),
                signal_type: "leave_voice".to_string(),
                data: "".to_string(),
            });
        }

        {
            let mut audio = self.audio_engine.lock().await;
            audio.reset();
        }

        self.playback_tx = None;
        self.is_muted.store(false, Ordering::Relaxed);
        self.room_id = None;

        let _ = self.event_tx.send(VoiceEvent::TxActivity(false));
        let _ = self.event_tx.send(VoiceEvent::MuteStateChanged(false));
        let _ = self.event_tx.send(VoiceEvent::Disconnected);

        Ok(())
    }

    pub async fn handle_signal(&mut self, _sender_id: &str, signal_type: &str, data: &str) -> Result<()> {
        match signal_type {
            "audio" => {
                if !self.is_joined.load(Ordering::Relaxed) {
                    return Ok(());
                }

                if let Some(ref tx) = self.playback_tx {
                    match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data) {
                        Ok(opus_packet) => {
                            let _ = tx.send(opus_packet);
                        }
                        Err(e) => {
                            let _ = self.event_tx.send(VoiceEvent::AudioError(format!("Failed to decode audio: {}", e)));
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
