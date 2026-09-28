use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

/// Thin wrapper around `kokorox::tts::koko::TTSKoko`.
///
/// Uses the `af_heart` English voice at 1× speed.  The kokorox crate handles
/// model + voices-file downloads automatically via its HuggingFace cache logic
/// when the files are not present at the given paths.
pub struct KokoroTts {
    inner: Arc<kokorox::tts::koko::TTSKoko>,
}

impl KokoroTts {
    /// Initialise from explicit paths.  If either file is absent, kokorox will
    /// attempt to download it from its default HuggingFace URL.
    pub async fn new(model_path: &Path, voices_path: &Path) -> anyhow::Result<Self> {
        let model_str = model_path.to_string_lossy();
        let voices_str = voices_path.to_string_lossy();

        // `from_paths` is async and performs network I/O if files are absent.
        let tts = kokorox::tts::koko::TTSKoko::from_paths(&model_str, &voices_str).await;

        Ok(Self {
            inner: Arc::new(tts),
        })
    }

    /// Synthesise `text` and return 24 kHz mono f32 PCM samples.
    pub fn synthesize(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.inner
            .tts_raw_audio(
                text,
                "en-us",
                "af_heart",
                1.0,           // speed
                None,          // initial_silence
                false,         // auto_detect_language
                false,         // force_style
                false,         // phonemes mode
            )
            .map_err(|e| anyhow::anyhow!("Kokoro synthesis failed: {e}"))
            .context("KokoroTts::synthesize")
    }

    /// Sample rate of the synthesised audio (Hz).
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
}

impl Clone for KokoroTts {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}
