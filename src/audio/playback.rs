use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::{FftFixedIn, Resampler};
use tracing::{error, info};

const TTS_SAMPLE_RATE: usize = 24_000;

#[derive(Clone)]
pub struct AudioPlayback {
    /// Raw 24 kHz mono samples waiting to be resampled.
    queue: Arc<Mutex<VecDeque<f32>>>,
    /// Device-rate mono samples ready for the cpal callback.
    resampled: Arc<Mutex<VecDeque<f32>>>,
    running: Arc<AtomicBool>,
    // Keep the stream alive for the lifetime of AudioPlayback.
    stream: Arc<Mutex<Option<cpal::Stream>>>,
}

impl AudioPlayback {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            resampled: Arc::new(Mutex::new(VecDeque::new())),
            running: Arc::new(AtomicBool::new(false)),
            stream: Arc::new(Mutex::new(None)),
        }
    }

    /// Append f32 PCM samples (24 kHz mono) to the playback queue.
    pub fn queue(&self, samples: Vec<f32>) {
        let mut q = self.queue.lock().unwrap();
        q.extend(samples);
    }

    /// Start the output stream. Safe to call multiple times — only opens stream once.
    pub fn start(&self) -> anyhow::Result<()> {
        let mut stream_lock = self.stream.lock().unwrap();
        if stream_lock.is_some() {
            return Ok(());
        }

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("No default output device")?;
        let config = device.default_output_config().context("No default output config")?;

        let native_rate = config.sample_rate().0 as usize;
        let channels = config.channels() as usize;

        info!(
            "Playback device: {}, sample rate: {}, channels: {}",
            device.name().unwrap_or_default(),
            native_rate,
            channels
        );

        let queue_ref = Arc::clone(&self.queue);
        let resampled_ref = Arc::clone(&self.resampled);
        self.running.store(true, Ordering::SeqCst);

        // Background thread: pulls from the 24 kHz queue, resamples to device
        // rate, and pushes into resampled_buf.  The cpal callback then reads
        // from resampled_buf at the device rate.
        let running = Arc::clone(&self.running);
        let resampled_producer = Arc::clone(&self.resampled);
        std::thread::spawn(move || {
            let chunk_size_in = 512usize;
            let mut resampler = FftFixedIn::<f32>::new(
                TTS_SAMPLE_RATE,
                native_rate,
                chunk_size_in,
                2,
                1,
            )
            .expect("Failed to build playback resampler");

            let mut pending: Vec<f32> = Vec::new();

            while running.load(Ordering::SeqCst) {
                // Drain from the main 24 kHz queue into pending.
                {
                    let mut q = queue_ref.lock().unwrap();
                    pending.extend(q.drain(..));
                }

                if pending.len() >= chunk_size_in {
                    let chunk: Vec<f32> = pending.drain(..chunk_size_in).collect();
                    let frames = vec![chunk];
                    match resampler.process(&frames, None) {
                        Ok(out) => {
                            let samples: Vec<f32> = out.into_iter().flatten().collect();
                            let mut rb = resampled_producer.lock().unwrap();
                            rb.extend(samples);
                        }
                        Err(e) => error!("Playback resample error: {e}"),
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        });

        // cpal output callback.  The resampled buffer contains mono samples at
        // the device rate.  For multi-channel output we duplicate each mono
        // sample to every channel in the frame — consuming exactly one
        // resampled sample per frame, not per output slot.
        let resampled_cb = Arc::clone(&resampled_ref);
        let stream = device.build_output_stream(
            &config.into(),
            move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut rb = resampled_cb.lock().unwrap();
                // output is interleaved: length = frames * channels
                for frame in output.chunks_mut(channels) {
                    let mono = rb.pop_front().unwrap_or(0.0);
                    for slot in frame.iter_mut() {
                        *slot = mono;
                    }
                }
            },
            move |err| {
                error!("Playback stream error: {err}");
            },
            None,
        )?;

        stream.play()?;
        *stream_lock = Some(stream);
        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let mut stream_lock = self.stream.lock().unwrap();
        *stream_lock = None;
    }

    /// Returns true only when both the raw input queue and the resampled
    /// output buffer are drained — i.e. all audio has been played.
    pub fn is_idle(&self) -> bool {
        let q = self.queue.lock().unwrap();
        let r = self.resampled.lock().unwrap();
        q.is_empty() && r.is_empty()
    }

    /// Immediately drain both the raw and resampled queues, silencing playback.
    pub fn clear(&self) {
        self.queue.lock().unwrap().clear();
        self.resampled.lock().unwrap().clear();
    }
}

impl Default for AudioPlayback {
    fn default() -> Self {
        Self::new()
    }
}
