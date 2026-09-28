use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("voicechat")
}

pub fn config_path() -> PathBuf {
    dirs::config_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("voicechat")
        .join("config.toml")
}

// ---------------------------------------------------------------------------
// STT (Whisper) model
// ---------------------------------------------------------------------------

pub fn whisper_model_path() -> PathBuf {
    data_dir().join("models").join("ggml-tiny.en.bin")
}

pub fn whisper_model_url() -> &'static str {
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
}

// ---------------------------------------------------------------------------
// TTS (Kokoro) model
// ---------------------------------------------------------------------------

pub fn tts_model_path() -> PathBuf {
    data_dir().join("tts").join("kokoro-v1.0.onnx")
}

pub fn tts_voices_path() -> PathBuf {
    data_dir().join("tts").join("voices-v1.0.bin")
}

// Kept for backward compatibility; callers that need the directory can use
// tts_voices_path().parent().
pub fn tts_voices_dir() -> PathBuf {
    data_dir().join("tts")
}

// ---------------------------------------------------------------------------
// Backend / preset config
// ---------------------------------------------------------------------------

/// A named CLI preset.  The backend spawns:
///   `command [args_before_prompt] "<prompt>" [args_after_prompt]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliPreset {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args_before_prompt: Vec<String>,
    #[serde(default)]
    pub args_after_prompt: Vec<String>,
    /// Optional environment variable to set when spawning (e.g. API key).
    #[serde(default)]
    pub env_key: Option<String>,
    #[serde(default)]
    pub env_value: Option<String>,
}

/// Top-level application config persisted to `config_path()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Name of the currently active preset.
    pub active_preset: String,
    pub presets: Vec<CliPreset>,
    /// Optional regex/substring for detecting Claude's prompt line.
    /// When `None`, the built-in heuristic is used (looks for ◆, "> ", "? ").
    #[serde(default)]
    pub prompt_regex: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_preset: "Claude Code".into(),
            presets: default_presets(),
            prompt_regex: None,
        }
    }
}

pub fn default_presets() -> Vec<CliPreset> {
    vec![
        CliPreset {
            name: "Claude Code".into(),
            command: "claude".into(),
            args_before_prompt: vec!["-p".into()],
            args_after_prompt: vec![],
            env_key: None,
            env_value: None,
        },
        CliPreset {
            name: "OpenAI Codex CLI".into(),
            command: "codex".into(),
            args_before_prompt: vec!["-q".into()],
            args_after_prompt: vec![],
            env_key: Some("OPENAI_API_KEY".into()),
            env_value: None, // filled in by user via settings
        },
        CliPreset {
            name: "Custom".into(),
            command: String::new(),
            args_before_prompt: vec![],
            args_after_prompt: vec![],
            env_key: None,
            env_value: None,
        },
    ]
}

/// Load the config from disk; falls back to defaults if missing or malformed.
pub fn load_config() -> AppConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => toml::from_str(&content).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

/// Persist the config to disk.
pub fn save_config(cfg: &AppConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(cfg)?;
    std::fs::write(&path, content)?;
    Ok(())
}

/// Return the active preset from `cfg`, falling back to the first preset or a
/// hardcoded default if the list is empty.
pub fn active_preset(cfg: &AppConfig) -> CliPreset {
    cfg.presets
        .iter()
        .find(|p| p.name == cfg.active_preset)
        .or_else(|| cfg.presets.first())
        .cloned()
        .unwrap_or_else(|| default_presets().into_iter().next().unwrap())
}
