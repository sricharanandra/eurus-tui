use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinVoicePayload {
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveVoicePayload {
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPayload {
    pub room_id: String,
    pub seq: u32,
    pub timestamp: u64,
    #[serde(rename = "payload")]
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerAudioPayload {
    pub room_id: String,
    pub user_id: String,
    pub seq: u32,
    pub timestamp: u64,
    #[serde(rename = "payload")]
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceJoinedPayload {
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceLeftPayload {
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceStatePayload {
    pub room_id: String,
    pub active_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "join_voice")]
    JoinVoice { payload: JoinVoicePayload },
    #[serde(rename = "leave_voice")]
    LeaveVoice { payload: LeaveVoicePayload },
    #[serde(rename = "audio")]
    Audio { payload: AudioPayload },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "voiceState")]
    VoiceState { payload: VoiceStatePayload },
    #[serde(rename = "voice_joined")]
    VoiceJoined { payload: VoiceJoinedPayload },
    #[serde(rename = "voice_left")]
    VoiceLeft { payload: VoiceLeftPayload },
    #[serde(rename = "audio")]
    Audio { payload: ServerAudioPayload },
}
