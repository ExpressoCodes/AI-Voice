#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    Idle,
    Listening,
    Transcribing,
    Thinking,
    Speaking,
    Error(String),
}

impl AppState {
    pub fn status_label(&self) -> &'static str {
        match self {
            AppState::Idle         => "Ready",
            AppState::Listening    => "Recording...",
            AppState::Transcribing => "Transcribing...",
            AppState::Thinking     => "Thinking...",
            AppState::Speaking     => "Speaking...",
            AppState::Error(_)     => "Error",
        }
    }

    pub fn show_spinner(&self) -> bool {
        matches!(self, AppState::Transcribing | AppState::Thinking)
    }

    pub fn show_waveform(&self) -> bool {
        matches!(self, AppState::Listening | AppState::Speaking)
    }

    pub fn show_cancel(&self) -> bool {
        matches!(self, AppState::Listening | AppState::Thinking | AppState::Speaking)
    }

    pub fn ptt_sensitive(&self) -> bool {
        matches!(self, AppState::Idle)
    }
}
