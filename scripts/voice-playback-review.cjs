// Compile and run the production playback callback without opening an audio device.
// The callback is extracted verbatim; only its unused CPAL metadata type is stubbed.
const assert = require('node:assert/strict');
const { readFileSync, writeFileSync, mkdtempSync } = require('node:fs');
const { tmpdir, homedir } = require('node:os');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');

const source = readFileSync(join(__dirname, '../src/voice/audio.rs'), 'utf8');
const start = source.indexOf('move |data: &mut [f32], _: &_|');
const end = source.indexOf('\n            err_fn,', start);
assert.ok(start >= 0 && end > start, 'production output callback must be present');
const callback = source.slice(start, end).trim().replace(/,$/, '').replace('_: &_|', '_: &()|');
const fixture = `
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

fn play(mono: &[f32], output: &mut [f32]) {
    let shared_buffer = Arc::new(Mutex::new(VecDeque::from(mono.to_vec())));
    let callback = ${callback};
    callback(output, &());
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
