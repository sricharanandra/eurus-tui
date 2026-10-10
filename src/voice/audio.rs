use anyhow::{anyhow, Result};
use audiopus::{coder::Decoder, coder::Encoder, Application, Channels, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamError, SupportedStreamConfig, SupportedStreamConfigRange};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Rates Opus handles natively, best first. 48k avoids resampling.
const PREFERRED_RATES: [u32; 5] = [48000, 24000, 16000, 12000, 8000];
/// Device sample formats we can convert to and from f32, best first.
const SUPPORTED_FORMATS: [SampleFormat; 3] = [SampleFormat::F32, SampleFormat::I16, SampleFormat::U16];
/// Max queued chunks/packets between audio stages; older audio is dropped
/// rather than piling up latency or memory.
const AUDIO_QUEUE_LEN: usize = 50;
/// How long a peer whose playback stream failed to build is left alone
/// before trying again (e.g. after the output device comes back).
const PLAYBACK_RETRY: Duration = Duration::from_secs(2);

// Wrapper to make cpal::Stream Send (required for tokio::spawn)
struct SendStream(cpal::Stream);
unsafe impl Send for SendStream {}

/// Audio errors that can be reported to the VoiceManager
#[derive(Debug, Clone)]
pub enum AudioDeviceError {
    /// Output device disconnected or failed
    OutputDeviceError(String),
    /// Input device disconnected or failed  
    InputDeviceError(String),
}

pub struct AudioEngine {
    input_stream: Option<SendStream>,
    /// Output streams keyed by peer_id for proper cleanup on peer disconnect
    output_streams: HashMap<String, SendStream>,
    /// Senders into each peer's decode task, keyed by peer_id.
    /// Reused across packets so the cpal output stream is created once per peer.
    peer_senders: HashMap<String, mpsc::Sender<Vec<u8>>>,
    /// Peers whose playback stream failed to build, with the failure time.
    /// Their packets are dropped until PLAYBACK_RETRY passes, avoiding
    /// 50 errors/sec while still recovering when the device returns.
    failed_peers: HashMap<String, Instant>,
    /// Channel to report audio errors back to VoiceManager
    error_tx: Option<mpsc::UnboundedSender<AudioDeviceError>>,
}

struct StatefulResampler {
    from_rate: u32,
    to_rate: u32,
    last_sample: f32,
    fraction: f32,
    samples_to_drop: usize, // How many samples to skip at start of next chunk
}

impl StatefulResampler {
    fn new(from: u32, to: u32) -> Self {
        Self { from_rate: from, to_rate: to, last_sample: 0.0, fraction: 0.0, samples_to_drop: 0 }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.from_rate == self.to_rate {
            return input.to_vec();
        }

        let ratio = self.from_rate as f32 / self.to_rate as f32;
        let mut output = Vec::with_capacity(input.len());
        
        // Start index relative to this buffer
        // -1 refers to self.last_sample
        // 0 refers to input[0]
        let mut idx: i32 = -1 + (self.samples_to_drop as i32);
        
        loop {
            // Check bounds for interpolation interval [idx, idx+1]
            if idx + 1 >= input.len() as i32 {
                break;
            }
            
            let s0 = if idx == -1 { self.last_sample } else { input[idx as usize] };
            let s1 = input[(idx + 1) as usize];
            
            let val = s0 + (s1 - s0) * self.fraction;
            output.push(val);
            
            self.fraction += ratio;
            while self.fraction >= 1.0 {
                self.fraction -= 1.0;
                idx += 1;
            }
        }
        
        // Calculate carry-over
        let new_idx = idx - (input.len() as i32);
        
        // We need to store `last_sample` regardless.
        if let Some(last) = input.last() {
            self.last_sample = *last;
        }
        
        // samples_to_drop = new_idx - (-1) = new_idx + 1
        self.samples_to_drop = (new_idx + 1) as usize;
        
        output
    }
}

/// Picks a supported config at the best Opus-friendly rate in a format we can
/// convert, falling back to the device default.
fn pick_config(
    ranges: Vec<SupportedStreamConfigRange>,
    default: Result<SupportedStreamConfig, cpal::DefaultStreamConfigError>,
) -> Result<SupportedStreamConfig> {
    for rate in PREFERRED_RATES {
        for format in SUPPORTED_FORMATS {
            if let Some(range) = ranges.iter().find(|r| {
                r.sample_format() == format && r.min_sample_rate().0 <= rate && r.max_sample_rate().0 >= rate
            }) {
                return Ok(range.clone().with_sample_rate(cpal::SampleRate(rate)));
            }
        }
    }
    let default = default?;
    if SUPPORTED_FORMATS.contains(&default.sample_format()) {
        Ok(default)
    } else {
        Err(anyhow!("Unsupported sample format {:?}", default.sample_format()))
    }
}

/// Fills an interleaved output buffer from mono samples: one sample per
/// frame, copied to every channel. Missing samples become silence.
fn fill_output(buffer: &Mutex<VecDeque<f32>>, out: &mut [f32], channels: usize) {
    let Ok(mut buffer) = buffer.lock() else {
        out.fill(0.0);
        return;
    };
    for frame in out.chunks_mut(channels.max(1)) {
        let sample = buffer.pop_front().unwrap_or(0.0);
        frame.fill(sample);
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    err_fn: impl FnMut(StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            scratch.resize(data.len(), 0.0);
            fill_output(&buffer, &mut scratch, channels);
            for (out, sample) in data.iter_mut().zip(&scratch) {
                *out = T::from_sample(*sample);
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    raw_tx: mpsc::Sender<Vec<f32>>,
    err_fn: impl FnMut(StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = (config.channels as usize).max(1);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // Downmix to mono by averaging channels
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32)
                .collect();
            // Drop the chunk if the encoder is behind
            let _ = raw_tx.try_send(mono);
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            input_stream: None,
            output_streams: HashMap::new(),
            peer_senders: HashMap::new(),
            failed_peers: HashMap::new(),
            error_tx: None,
        }
    }

    /// Set the error channel for reporting audio device errors
    pub fn set_error_channel(&mut self, tx: mpsc::UnboundedSender<AudioDeviceError>) {
        self.error_tx = Some(tx);
    }

    /// Reset all audio streams - must be called before rejoining voice
    pub fn reset(&mut self) {
        // Drop input stream (stops capture)
        self.input_stream.take();
        self.drop_playback();
    }

    /// Drop all output streams and their senders. The next packet for each
    /// peer rebuilds its stream, which is how playback recovers from device errors.
    pub fn drop_playback(&mut self) {
        self.output_streams.clear();
        self.peer_senders.clear();
        self.failed_peers.clear();
    }

    /// Remove the output stream associated with a specific peer.
    /// Called when a peer disconnects or leaves voice to prevent stream accumulation.
    pub fn remove_peer_stream(&mut self, peer_id: &str) {
        self.output_streams.remove(peer_id);
        self.peer_senders.remove(peer_id);
    }

    /// Push a single Opus packet for a peer. Lazily creates the peer's
    /// decode + playback stream on first packet, then reuses the sender
    /// for all subsequent packets. This avoids recreating the cpal stream
    /// every 20ms (which produced silence).
    pub fn push_peer_packet(&mut self, peer_id: &str, packet: Vec<u8>) -> Result<()> {
        if let Some(failed_at) = self.failed_peers.get(peer_id) {
            if failed_at.elapsed() < PLAYBACK_RETRY {
                return Ok(());
            }
            self.failed_peers.remove(peer_id);
        }
        if let Some(tx) = self.peer_senders.get(peer_id) {
            // Full means the decoder is behind; dropping keeps latency bounded
            let _ = tx.try_send(packet);
            return Ok(());
        }

        let (packet_tx, packet_rx) = mpsc::channel::<Vec<u8>>(AUDIO_QUEUE_LEN);
        let stream = match self.build_playback_stream(packet_rx) {
            Ok(s) => s,
            Err(e) => {
                self.failed_peers.insert(peer_id.to_string(), Instant::now());
                return Err(e);
            }
        };
        self.output_streams.insert(peer_id.to_string(), SendStream(stream));
        let _ = packet_tx.try_send(packet);
        self.peer_senders.insert(peer_id.to_string(), packet_tx);
        Ok(())
    }

    /// Build and start a cpal output stream that decodes Opus packets from packet_rx.
    fn build_playback_stream(&self, mut packet_rx: mpsc::Receiver<Vec<u8>>) -> Result<cpal::Stream> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(anyhow!("No output device"))?;

        let config = pick_config(
            device.supported_output_configs()?.collect(),
            device.default_output_config(),
        )?;
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();
        let device_sample_rate = stream_config.sample_rate.0;

        // Opus only supports specific rates. We decode to 48k and resample if needed.
        let opus_rate = SampleRate::Hz48000;

        // Resampler: 48k -> device_rate
        let mut resampler = StatefulResampler::new(48000, device_sample_rate);

        // Mono samples; capped at 500ms so a backlog can't add lasting latency
        let max_buffer_samples = device_sample_rate as usize / 2;
        let shared_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(max_buffer_samples)));

        // Spawn Decoding Task
        let buffer_for_decode = shared_buffer.clone();
        tokio::spawn(async move {
            let mut decoder = match Decoder::new(opus_rate, Channels::Mono) {
                Ok(d) => d,
                Err(_) => return,
            };

            while let Some(packet) = packet_rx.recv().await {
                let mut output = [0.0f32; 1920]; // 40ms at 48k
                if let Ok(len) = decoder.decode_float(Some(&packet), &mut output[..], false) {
                    let decoded_frames = &output[..len];
                    // Resample if needed
                    let resampled = resampler.process(decoded_frames);

                    if let Ok(mut buffer) = buffer_for_decode.lock() {
                        buffer.extend(resampled);
                        // Prevent bufferbloat / drift
                        if buffer.len() > max_buffer_samples {
                            let drain_count = buffer.len() - max_buffer_samples;
                            buffer.drain(0..drain_count);
                        }
                    }
                }
            }
        });

        // Setup CPAL Output Stream with proper error handling
        let error_tx_for_output = self.error_tx.clone();
        let err_fn = move |err: StreamError| {
            if let Some(tx) = &error_tx_for_output {
                let _ = tx.send(AudioDeviceError::OutputDeviceError(format!("{:?}", err)));
            }
        };

        let stream = match sample_format {
            SampleFormat::F32 => build_output::<f32>(&device, &stream_config, shared_buffer, err_fn)?,
            SampleFormat::I16 => build_output::<i16>(&device, &stream_config, shared_buffer, err_fn)?,
            SampleFormat::U16 => build_output::<u16>(&device, &stream_config, shared_buffer, err_fn)?,
            other => return Err(anyhow!("Unsupported output sample format {:?}", other)),
        };

        stream.play()?;
        Ok(stream)
    }

    pub fn start_capture(&mut self, encoded_tx: mpsc::Sender<(Vec<u8>, bool)>) -> Result<()> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(anyhow!("No input device"))?;

        let config = pick_config(
            device.supported_input_configs()?.collect(),
            device.default_input_config(),
        )?;
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();
        let device_sample_rate = stream_config.sample_rate.0;

        // Guard against double-capture: stop any existing input stream first.
        self.input_stream.take();

        // We encode at 48k. Resample input -> 48k.
        let opus_rate = SampleRate::Hz48000;
        let frame_size_48k = (48000 * 20) / 1000; // 960 samples for 20ms

        // Resampler: device_rate -> 48k
        let mut resampler = StatefulResampler::new(device_sample_rate, 48000);

        // Channel from CPAL -> Encoder
        let (raw_tx, mut raw_rx) = mpsc::channel::<Vec<f32>>(AUDIO_QUEUE_LEN);

        // Spawn Encoding Task
        tokio::spawn(async move {
            let encoder = match Encoder::new(opus_rate, Channels::Mono, Application::Voip) {
                Ok(e) => e,
                Err(_) => return,
            };

            // We need to buffer incoming resampled samples until we have a full Opus frame (960 samples)
            let mut buffer: Vec<f32> = Vec::with_capacity(frame_size_48k * 2);

            while let Some(samples) = raw_rx.recv().await {
                // Resample incoming chunk
                let resampled = resampler.process(&samples);
                buffer.extend(resampled);

                while buffer.len() >= frame_size_48k {
                    let frame: Vec<f32> = buffer.drain(0..frame_size_48k).collect();
                    let mut output = [0u8; 1024];

                    if let Ok(len) = encoder.encode_float(&frame, &mut output) {
                        let packet = output[..len].to_vec();
                        // VAD-lite: RMS energy threshold
                        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
                        let active = rms > 0.01;
                        match encoded_tx.try_send((packet, active)) {
                            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                        }
                    }
                }
            }
        });

        // Setup CPAL Input Stream with proper error handling
        let error_tx_for_input = self.error_tx.clone();
        let err_fn = move |err: StreamError| {
            if let Some(tx) = &error_tx_for_input {
                let _ = tx.send(AudioDeviceError::InputDeviceError(format!("{:?}", err)));
            }
        };

        let stream = match sample_format {
            SampleFormat::F32 => build_input::<f32>(&device, &stream_config, raw_tx, err_fn)?,
            SampleFormat::I16 => build_input::<i16>(&device, &stream_config, raw_tx, err_fn)?,
            SampleFormat::U16 => build_input::<u16>(&device, &stream_config, raw_tx, err_fn)?,
            other => return Err(anyhow!("Unsupported input sample format {:?}", other)),
        };

        stream.play()?;
        self.input_stream = Some(SendStream(stream));

        Ok(())
    }
}
