// Exercise production command and server-message handlers without running the UI.
include!("../src/main.rs");

#[cfg(test)]
mod review {
    use super::*;

    async fn check_room_switch(command: &str) {
        let mut app = App::default();
        app.current_screen = CurrentScreen::InRoom;
        app.room_id = Some("old-room".to_string());
        app.voice.status = VoiceConnectionStatus::Connected;
        let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
        let (voice_tx, mut voice_rx) = mpsc::unbounded_channel();
        app.ws_sender = Some(ws_tx);
        app.voice_tx = Some(voice_tx);
        execute_command(&mut app, command).await;
        assert!(ws_rx.try_recv().is_ok(), "room switch must actually be sent");
        let joined: ServerMessage = serde_json::from_value(serde_json::json!({
            "type": "roomJoined",
            "payload": {"roomId":"new-room", "roomName":"new-room",
                "displayName":"new-room", "roomType":"public", "encryptedKey":"",
                "messages":[], "onlineUsers":[]}
        })).expect("valid server room confirmation");
        handle_server_message(&mut app, joined);
        let mut confirmed = false;
        while let Ok(command) = voice_rx.try_recv() {
            if let voice::manager::VoiceCommand::RoomJoined { room_id } = command {
                confirmed = room_id == "new-room";
            }
        }
        // The voice manager ends voice in old-room when told about the new room
        assert!(confirmed, "{command} changed rooms without telling voice which room it is now in");
    }

    #[tokio::test]
    async fn invite_switch_ends_old_voice_capture() {
        check_room_switch("join invite-code").await;
    }

    #[tokio::test]
    async fn dm_switch_ends_old_voice_capture() {
        check_room_switch("dm other-user").await;
    }
}
