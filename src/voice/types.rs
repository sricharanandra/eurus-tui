use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinVoicePayload {
    #[serde(rename = "roomId")]
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveVoicePayload {
    #[serde(rename = "roomId")]
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPayload {
    #[serde(rename = "roomId")]
    pub room_id: String,
    pub seq: u32,
    pub timestamp: u64,
    #[serde(rename = "payload")]
    pub data: String,
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
