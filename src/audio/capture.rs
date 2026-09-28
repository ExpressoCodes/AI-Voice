use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::{FftFixedIn, Resampler};
use tokio::sync::mpsc;
use tracing::{error, info};

// cpal::Stream is not Send on all platforms (it wraps a raw pointer on Linux/ALSA).
// We only ever access the stream from the single thread it's moved into, so this is safe.
struct SendStream(cpal::Stream);
unsafe impl Send for SendStream {}

const TARGET_SAMPLE_RATE: u32 = 16_000;
// 20 ms of audio at 16 kHz
const CHUNK_FRAMES: usize = 320;

pub struct AudioCapture {
    running: Arc<AtomicBool>,
}

impl AudioCapture {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start capturing audio. Returns a receiver of 20 ms chunks at 16 kHz mono.
    pub fn start(&self) -> anyhow::Result<mpsc::Receiver<Vec<f32>>> {
        let (tx, rx) = mpsc::channel::<Vec<f32>>(64);
        let running = Arc::clone(&self.running);
        running.store(true, Ordering::SeqCst);

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No default input device")?;
        let config = device.default_input_config().context("No default input config")?;

        info!(
            "Capture device: {}, sample rate: {}, channels: {}",
            device.name().unwrap_or_default(),
            config.sample_rate().0,
            config.channels()
        );

        let native_sample_rate = config.sample_rate().0 as usize;
        let channels = config.channels() as usize;

        // Buffer to accumulate interleaved samples from cpal callback
        let raw_buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let raw_buf_cb = Arc::clone(&raw_buf);

        let running_cb = Arc::clone(&running);
        let tx_cb = tx.clone();

        // We'll run the resampler in a separate thread
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<Vec<f32>>();

        let stream = {
            let raw_buf_cb2 = Arc::clone(&raw_buf_cb);
            device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !running_cb.load(Ordering::SeqCst) {
                        return;
                    }
                    let mut buf = raw_buf_cb2.lock().unwrap();
                    buf.extend_from_slice(data);
                    // Drain full frames worth of data
                    let frame_size = channels;
                    let frames_available = buf.len() / frame_size;
                    if frames_available > 0 {
                        let drain_len = frames_available * frame_size;
                        let chunk: Vec<f32> = buf.drain(..drain_len).collect();
                        let _ = raw_tx.send(chunk);
                    }
                },
                move |err| {
                    error!("Capture stream error: {err}");
                },
                None,
            )?
        };

        stream.play()?;

        // Keep stream alive by moving into thread.
        // Wrap in SendStream because cpal::Stream is not Send on Linux/ALSA,
        // but it is safe to keep alive on any single thread.
        let stream = SendStream(stream);
        let running_thread = Arc::clone(&self.running);
        std::thread::spawn(move || {
            let _stream = stream; // keep alive

            // Resampler: native -> 16 kHz
            let resample_ratio = TARGET_SAMPLE_RATE as f64 / native_sample_rate as f64;
            // chunk_size_in: how many native frames we feed at once to get CHUNK_FRAMES out
            let chunk_size_in = (CHUNK_FRAMES as f64 / resample_ratio).ceil() as usize;

            let mut resampler = FftFixedIn::<f32>::new(
                native_sample_rate,
                TARGET_SAMPLE_RATE as usize,
                chunk_size_in,
                2,
                1,
            )
            .expect("Failed to build resampler");

            let mut mono_buf: Vec<f32> = Vec::new();

            while running_thread.load(Ordering::SeqCst) {
                match raw_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(interleaved) => {
                        // Downmix to mono
                        let frames = interleaved.len() / channels;
                        for f in 0..frames {
                            let sample = (0..channels)
                                .map(|c| interleaved[f * channels + c])
                                .sum::<f32>()
                                / channels as f32;
                            mono_buf.push(sample);
                        }

                        // Process full resampler chunks
                        while mono_buf.len() >= chunk_size_in {
                            let input_chunk: Vec<f32> =
                                mono_buf.drain(..chunk_size_in).collect();
                            let input_frames = vec![input_chunk];
                            match resampler.process(&input_frames, None) {
                                Ok(out) => {
                                    let samples = out.into_iter().flatten().collect::<Vec<f32>>();
                                    if tx_cb.blocking_send(samples).is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    error!("Resampler error: {e}");
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                }
            }
        });

        Ok(rx)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Default for AudioCapture {
    fn default() -> Self {
        Self::new()
    }
}
