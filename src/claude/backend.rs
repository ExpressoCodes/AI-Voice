use futures::Stream;
use std::pin::Pin;

/// A single turn in the conversation history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// Trait implemented by every AI backend.
///
/// The trait is object-safe via `Pin<Box<dyn Stream>>` so it can be used as
/// `Arc<dyn Backend>`.
pub trait Backend: Send + Sync {
    /// Stream a response given the full conversation history and the new user
    /// turn. Returns a stream of text chunks (not necessarily line-aligned).
    fn stream_message(
        &self,
        history: &[Message],
        user_text: &str,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send>>>;
}
