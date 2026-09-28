use std::collections::HashMap;
use std::pin::Pin;
use std::process::Stdio;

use anyhow::Context;
use async_stream::try_stream;
use futures::Stream;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::claude::backend::{Backend, Message};

/// Kills the child process on drop so that closing the app mid-inference
/// does not leave an orphan `claude` / `codex` process running.
struct KillOnDrop(tokio::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // Best-effort kill — ignore errors (process may have already exited)
        let _ = self.0.start_kill();
    }
}

/// Generic CLI subprocess backend.
///
/// Spawns `command [args_before_prompt] "<prompt>" [args_after_prompt]` and
/// streams stdout line-by-line. Works for any AI CLI that accepts a prompt as
/// a positional argument and writes its response to stdout (e.g. `claude`,
/// `codex`, `sgpt`, `oai`).
pub struct CliBackend {
    command: String,
    args_before_prompt: Vec<String>,
    args_after_prompt: Vec<String>,
    env_vars: HashMap<String, String>,
}

impl CliBackend {
    pub fn new(
        command: impl Into<String>,
        args_before_prompt: Vec<String>,
        args_after_prompt: Vec<String>,
    ) -> Self {
        Self {
            command: command.into(),
            args_before_prompt,
            args_after_prompt,
            env_vars: HashMap::new(),
        }
    }

    /// Set an environment variable that will be passed to the subprocess.
    /// Useful for API keys (e.g., `OPENAI_API_KEY`).
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Build the full prompt string from history + new user turn.
    fn build_prompt(history: &[Message], user_text: &str) -> String {
        let mut prompt = String::new();
        for msg in history {
            let role = if msg.role == "user" { "User" } else { "Assistant" };
            prompt.push_str(&format!("{}: {}\n\n", role, msg.content));
        }
        prompt.push_str(&format!("User: {}\n\nAssistant:", user_text));
        prompt
    }
}

impl Backend for CliBackend {
    fn stream_message(
        &self,
        history: &[Message],
        user_text: &str,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send>>> {
        let prompt = Self::build_prompt(history, user_text);

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args_before_prompt);
        cmd.arg(&prompt);
        cmd.args(&self.args_after_prompt);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit()); // surface errors (e.g. unknown flags) instead of silently swallowing them

        for (k, v) in &self.env_vars {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "Failed to spawn `{}` — is it installed and on PATH?",
                self.command
            )
        })?;

        let mut stdout = child.stdout.take().context("subprocess had no stdout")?;

        let stream = try_stream! {
            // Keep the child alive for the duration of the stream and kill it
            // if the stream is dropped early (e.g. window closed mid-inference).
            let _child = KillOnDrop(child);
            // Read in small chunks so the UI updates as soon as bytes arrive
            // rather than waiting for a full newline.
            let mut buf = vec![0u8; 64];
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 { break; }
                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                if !chunk.trim().is_empty() {
                    yield chunk;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
