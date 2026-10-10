// Regression checks for the voice lifecycle findings in docs/voice-review.md.
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

            struct Harness {
                manager: VoiceManager,
                events: mpsc::UnboundedReceiver<VoiceEvent>,
                sent: mpsc::UnboundedReceiver<String>,
            }

            impl Harness {
                fn new() -> Self {
                    let (event_tx, events) = mpsc::unbounded_channel();
                    let (ws_tx, sent) = mpsc::unbounded_channel();
                    let mut manager = VoiceManager::new(event_tx);
                    manager.ws_tx = Some(ws_tx);
                    manager.connected = true;
                    Harness { manager, events, sent }
                }

                /// A live call in old-room without touching audio devices.
                fn live() -> Self {
                    let mut h = Self::new();
                    h.manager.session = Session::Live {
                        room_id: "old-room".to_string(),
                        mic_task: tokio::spawn(async {}),
                        capture_started: Instant::now(),
                    };
                    h
                }

                fn sent_types(&mut self) -> Vec<String> {
                    let mut types = Vec::new();
                    while let Ok(json) = self.sent.try_recv() {
                        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
                        types.push(value["type"].as_str().unwrap_or_default().to_string());
                    }
                    types
                }

                fn events(&mut self) -> Vec<VoiceEvent> {
                    let mut events = Vec::new();
                    while let Ok(event) = self.events.try_recv() {
                        events.push(event);
                    }
                    events
                }
            }

            #[tokio::test]
            async fn explicit_leave_cancels_rejoin_after_disconnect() {
                let mut h = Harness::live();
                h.manager.handle_connection_status(false);
                assert!(matches!(h.manager.session, Session::Suspended { .. }));
                h.manager.handle_leave();
                h.manager.handle_connection_status(true);
                h.sent_types();
                h.manager.handle_room_joined("old-room".to_string());
                assert!(matches!(h.manager.session, Session::Idle));
                assert!(h.sent_types().is_empty(), "explicit leave still rejoined old-room");
            }

            #[tokio::test]
            async fn transport_disconnect_preserves_mute_for_auto_rejoin() {
                let mut h = Harness::live();
                h.manager.is_muted.store(true, Ordering::Relaxed);
                h.manager.handle_connection_status(false);
                h.manager.handle_connection_status(true);
                h.manager.handle_room_joined("old-room".to_string());
                assert!(matches!(h.manager.session, Session::Joining { .. }));
                assert_eq!(h.sent_types(), vec!["join_voice"]);
                assert!(h.manager.is_muted.load(Ordering::Relaxed),
                    "automatic rejoin will capture with mute cleared");
            }

            #[tokio::test]
            async fn ordinary_leave_clears_session_intent() {
                let mut h = Harness::live();
                h.manager.handle_leave();
                assert!(matches!(h.manager.session, Session::Idle));
                assert_eq!(h.sent_types(), vec!["leave_voice"]);
            }

            #[tokio::test]
            async fn room_change_ends_voice_in_previous_room() {
                let mut h = Harness::live();
                h.manager.handle_room_joined("new-room".to_string());
                assert!(matches!(h.manager.session, Session::Idle));
                assert_eq!(h.sent_types(), vec!["leave_voice"]);
                assert!(h.events().iter().any(|e| matches!(e, VoiceEvent::VoiceLeft)));
            }

            #[tokio::test]
            async fn same_room_confirmation_keeps_voice() {
                let mut h = Harness::live();
                h.manager.handle_room_joined("old-room".to_string());
                assert!(matches!(h.manager.session, Session::Live { .. }));
                assert!(h.sent_types().is_empty());
            }

            #[tokio::test]
            async fn join_waits_for_server_admission() {
                let mut h = Harness::new();
                h.manager.handle_join("room".to_string());
                assert_eq!(h.sent_types(), vec!["join_voice"]);
                let events = h.events();
                assert!(events.iter().any(|e| matches!(e, VoiceEvent::Connecting)));
                assert!(!events.iter().any(|e| matches!(e, VoiceEvent::Connected)),
                    "reported Connected before the server admitted the join");

                h.manager.handle_voice_error("room".to_string(), "denied".to_string());
                assert!(matches!(h.manager.session, Session::Idle));
                assert!(h.events().iter().any(|e| matches!(e, VoiceEvent::ConnectionFailed(_))));
            }

            #[tokio::test]
            async fn join_without_socket_fails_instead_of_connecting() {
                let mut h = Harness::new();
                h.manager.connected = false;
                h.manager.handle_join("room".to_string());
                assert!(matches!(h.manager.session, Session::Idle));
                assert!(h.events().iter().any(|e| matches!(e, VoiceEvent::ConnectionFailed(_))));
            }

            #[tokio::test]
            async fn leave_while_idle_is_silent() {
                let mut h = Harness::new();
                h.manager.handle_leave();
                assert!(h.events().is_empty(), "idle leave overwrote UI status");
                assert!(h.sent_types().is_empty());
            }

            #[tokio::test]
            async fn stale_voice_ack_is_ignored() {
                let mut h = Harness::new();
                h.manager.handle_voice_joined("room".to_string());
                assert!(matches!(h.manager.session, Session::Idle));
            }
        }
    }
}
