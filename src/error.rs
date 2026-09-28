use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Audio error: {0}")]
    Audio(String),
    #[error("STT error: {0}")]
    Stt(String),
    #[error("TTS error: {0}")]
    Tts(String),
    #[error("Claude API error: {0}")]
    Claude(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Model download error: {0}")]
    Download(String),
}
