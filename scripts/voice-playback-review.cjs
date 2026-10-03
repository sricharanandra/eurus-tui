// Compile and run the production playback fill function without opening an audio device.
// The function is extracted verbatim from audio.rs.
const assert = require('node:assert/strict');
const { readFileSync, writeFileSync, mkdtempSync } = require('node:fs');
const { tmpdir, homedir } = require('node:os');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');

const source = readFileSync(join(__dirname, '../src/voice/audio.rs'), 'utf8');
const start = source.indexOf('fn fill_output(');
const end = source.indexOf('\n}\n', start) + 2;
assert.ok(start >= 0 && end > start, 'production fill_output must be present');
const fillOutput = source.slice(start, end);
const fixture = `
use std::sync::Mutex;
use std::collections::VecDeque;

${fillOutput}

fn play(mono: &[f32], output: &mut [f32]) {
    let channels = if output.len() > mono.len() { output.len() / mono.len() } else { 1 };
    fill_output(&Mutex::new(VecDeque::from(mono.to_vec())), output, channels);
}

#[test]
fn mono_output_preserves_samples() {
    let mono = [0.1, 0.2, 0.3, 0.4];
    let mut output = [0.0; 4];
    play(&mono, &mut output);
    assert_eq!(output, mono);
}

#[test]
fn stereo_output_duplicates_each_mono_sample_across_channels() {
    let mono = [0.1, 0.2, 0.3, 0.4];
    let mut output = [0.0; 8];
    play(&mono, &mut output);
    assert_eq!(output, [0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4]);
}
`;
const dir = mkdtempSync(join(tmpdir(), 'eurus-playback-review-'));
const rustSource = join(dir, 'callback.rs');
const binary = join(dir, 'callback-test');
writeFileSync(rustSource, fixture);
console.log(`Production audio.rs SHA256: ${createHash('sha256').update(source).digest('hex')}`);
const rustc = process.env.RUSTC || join(homedir(), '.cargo/bin/rustc');
const compiled = spawnSync(rustc, ['--test', rustSource, '-o', binary], { stdio: 'inherit' });
if (compiled.error) throw compiled.error;
if (compiled.status !== 0) process.exit(compiled.status ?? 1);
const result = spawnSync(binary, [], { stdio: 'inherit' });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
