use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperStt {
    ctx: Arc<Mutex<WhisperContext>>,
}

impl WhisperStt {
    pub fn new(model_path: &Path) -> anyhow::Result<Self> {
        let path_str = model_path
            .to_str()
            .context("Model path is not valid UTF-8")?;

        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| anyhow::anyhow!("Failed to load Whisper model: {e:?}"))?;

        Ok(Self {
            ctx: Arc::new(Mutex::new(ctx)),
        })
    }

    /// Run inference synchronously. Call via `tokio::task::spawn_blocking` from async code.
    pub fn transcribe(&self, samples: Vec<f32>) -> anyhow::Result<String> {
        let ctx = self.ctx.lock().unwrap();
        let mut state = ctx
            .create_state()
            .map_err(|e| anyhow::anyhow!("Failed to create Whisper state: {e:?}"))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // Performance tuning
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4)
            .min(4); // whisper tiny peaks around 4 threads; more adds overhead
        params.set_n_threads(n_threads);
        // set_speed_up not available in whisper-rs 0.14.4 — skipped
        params.set_translate(false);
        params.set_single_segment(true);
        params.set_max_tokens(128);
        params.set_audio_ctx(512);

        state
            .full(params, &samples)
            .map_err(|e| anyhow::anyhow!("Whisper inference failed: {e:?}"))?;

        let n_segments = state
            .full_n_segments()
            .map_err(|e| anyhow::anyhow!("Failed to get segment count: {e:?}"))?;

        let mut result = String::new();
        for i in 0..n_segments {
            let text = state
                .full_get_segment_text(i)
                .map_err(|e| anyhow::anyhow!("Failed to get segment text: {e:?}"))?;
            result.push_str(&text);
        }

        Ok(result.trim().to_string())
    }
}

impl Clone for WhisperStt {
    fn clone(&self) -> Self {
        Self {
            ctx: Arc::clone(&self.ctx),
        }
    }
}
