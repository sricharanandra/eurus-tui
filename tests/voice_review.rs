// Audit checks assert desired behavior and fail while the findings remain open.
// Includes production modules unchanged; no audio device or server is started.
#[path = "../src/api.rs"]
mod api;

mod voice {
    pub mod debug { include!("../src/voice/debug.rs"); }
    pub mod types { include!("../src/voice/types.rs"); }
    pub mod audio { include!("../src/voice/audio.rs"); }
    pub mod manager {
        include!("../src/voice/manager.rs");

        #[cfg(test)]
        mod review {
            use super::*;

            async fn connected_voice() -> VoiceManager {
                let (events, _receiver) = mpsc::unbounded_channel();
                let manager = VoiceManager::new(events);
                *manager.state.lock().await = VoiceState::Streaming;
                *manager.room_id.lock().await = Some("old-room".to_string());
                manager.in_voice.store(true, Ordering::Relaxed);
                manager.was_in_voice.store(true, Ordering::Relaxed);
                manager
            }

            #[tokio::test]
            async fn explicit_leave_cancels_rejoin_after_disconnect() {
                let manager = connected_voice().await;
                manager.handle_connection_status(false).await;
                manager.handle_leave().await;
                assert!(!manager.was_in_voice.load(Ordering::Relaxed),
                    "explicit leave still allows automatic rejoin to old-room");
                assert!(manager.room_id.lock().await.is_none());
            }

            #[tokio::test]
            async fn transport_disconnect_preserves_mute_for_auto_rejoin() {
                let manager = connected_voice().await;
                manager.is_muted.store(true, Ordering::Relaxed);
                manager.handle_connection_status(false).await;
                assert!(manager.was_in_voice.load(Ordering::Relaxed));
                assert!(manager.is_muted.load(Ordering::Relaxed),
                    "automatic rejoin will capture with mute cleared");
            }

            #[tokio::test]
            async fn ordinary_leave_clears_session_intent() {
                let manager = connected_voice().await;
                manager.handle_leave().await;
                assert!(!manager.in_voice.load(Ordering::Relaxed));
                assert!(!manager.was_in_voice.load(Ordering::Relaxed));
                assert!(manager.room_id.lock().await.is_none());
            }
        }
    }
}
