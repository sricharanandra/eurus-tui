// Replay one second of arrivals through the actual UI receive block and handlers.
// Virtual 50ms UI ticks avoid terminal timing and network nondeterminism.
const assert = require('node:assert/strict');
const { readFileSync, writeFileSync, unlinkSync } = require('node:fs');
const { homedir } = require('node:os');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');

const root = join(__dirname, '..');
const source = readFileSync(join(root, 'src/main.rs'), 'utf8');
const start = source.indexOf('        // Handle incoming WebSocket messages without blocking UI');
const end = source.indexOf('        // Handle Voice Events', start);
assert.ok(start >= 0 && end > start, 'production receive block must be present');
assert.ok(source.includes('event::poll(std::time::Duration::from_millis(50))'), 'review tick must match production');
const receive = source.slice(start, end);
const generated = join(root, 'tests/__voice_receive_review.rs');
writeFileSync(generated, `
include!("../src/main.rs");

#[test]
fn review_receive_keeps_up_with_fifty_voice_packets_per_second() {
    let mut state = App::default();
    let app = &mut state;
    app.room_id = Some("room".to_string());
    let (voice_tx, mut voice_rx) = mpsc::unbounded_channel();
    app.voice_tx = Some(voice_tx);
    let (ws_incoming_tx, mut ws_incoming_rx) = mpsc::unbounded_channel();
    let mut sent = 0;
    for tick in 0..20 {
        // 20ms server packets arrive at alternating batches of 2 and 3 per 50ms.
        for _ in 0..if tick % 2 == 0 { 2 } else { 3 } {
            ws_incoming_tx.send(serde_json::json!({"type":"audio", "payload": {
                "roomId":"room", "userId":"listener", "seq":sent,
                "timestamp":sent * 20, "payload":""}}).to_string()).expect("receiver open");
            sent += 1;
        }
        ${receive}
    }
    let mut forwarded = 0;
    while let Ok(command) = voice_rx.try_recv() {
        if matches!(command, voice::manager::VoiceCommand::ServerAudio(_)) { forwarded += 1; }
    }
    assert_eq!(sent, 50);
    assert_eq!(forwarded, sent, "voice backlog grows every second while the terminal is idle");
}
`, { flag: 'wx' });
try {
  const cargo = process.env.CARGO || join(homedir(), '.cargo/bin/cargo');
  const result = spawnSync(cargo, ['test', '--locked', '--test', '__voice_receive_review', 'review_receive_'], { cwd: root, stdio: 'inherit' });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally {
  unlinkSync(generated);
}
