use std::collections::HashMap;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_stream::try_stream;
use futures::Stream;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::claude::backend::{Backend, Message};

/// Kills the child process on drop so that closing the app mid-inference
/// does not leave an orphan `claude` process running.
struct KillOnDrop(tokio::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// Describes a pending tool-use permission request from Claude.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    /// Human-readable summary of the most relevant input field
    /// (e.g. "command: rm -rf /tmp/test" for Bash, "path: /etc/hosts" for Edit).
    pub input_summary: String,
}

/// Claude Code CLI backend with:
/// - `--output-format stream-json --include-partial-messages` for per-token streaming
/// - `--input-format stream-json --permission-prompt-tool stdio` for tool permission handling
/// - Session tracking via `--resume <session-id>` to avoid resending history each turn
pub struct ClaudeCodeBackend {
    /// Session ID from the last completed turn. Protected by Mutex for interior
    /// mutability since `stream_message` takes `&self`.
    session_id: Arc<Mutex<Option<String>>>,
    /// Channel sender for forwarding permission requests to the UI.
    permission_tx: Arc<Mutex<Option<mpsc::UnboundedSender<PermissionRequest>>>>,
    /// One-shot senders keyed by request_id; resolved when the UI responds.
    pending_permissions: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl ClaudeCodeBackend {
    pub fn new() -> Self {
        Self {
            session_id: Arc::new(Mutex::new(None)),
            permission_tx: Arc::new(Mutex::new(None)),
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register the channel over which permission requests will be forwarded to
    /// the UI. Call this once after constructing the backend.
    pub fn set_permission_channel(&self, tx: mpsc::UnboundedSender<PermissionRequest>) {
        *self.permission_tx.lock().unwrap() = Some(tx);
    }

    /// Resolve a pending permission request identified by `request_id`.
    /// `allow = true` → "allow", `allow = false` → "deny".
    pub fn respond_to_permission(&self, request_id: String, allow: bool) {
        if let Some(tx) = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(&request_id)
        {
            let _ = tx.send(allow);
        }
    }

    /// Build a single-string prompt that includes all history and the new user
    /// turn. Used only on the first turn when no session ID is available.
    fn build_first_turn_prompt(history: &[Message], user_text: &str) -> String {
        if history.is_empty() {
            return user_text.to_string();
        }
        let mut prompt = String::new();
        for msg in history {
            let role = if msg.role == "user" { "User" } else { "Assistant" };
            prompt.push_str(&format!("{}: {}\n\n", role, msg.content));
        }
        prompt.push_str(&format!("User: {}\n\nAssistant:", user_text));
        prompt
    }
}

impl Default for ClaudeCodeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for ClaudeCodeBackend {
    fn stream_message(
        &self,
        history: &[Message],
        user_text: &str,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send>>> {
        // Clone Arcs so the stream closure owns them without holding a reference
        // to self (which may be dropped before the stream completes).
        let session_id_arc = Arc::clone(&self.session_id);
        let permission_tx_arc = Arc::clone(&self.permission_tx);
        let pending_permissions_arc = Arc::clone(&self.pending_permissions);
        let existing_session = session_id_arc.lock().unwrap().clone();

        let mut cmd = Command::new("claude");

        // stream-json with partial messages gives us one JSON line per token
        // delta, so the UI can update character-by-character instead of waiting
        // for the subprocess to emit a newline.
        // --permission-prompt-tool stdio enables the control_request / control_response
        // protocol for tool permissions over stdout/stdin.
        cmd.args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ]);

        let prompt = if let Some(ref id) = existing_session {
            // Session continuation: claude already holds the history, so we
            // only need to send the new user message.
            cmd.args(["--resume", id]);
            user_text.to_string()
        } else {
            // First turn (no session): pack the full history into the prompt.
            Self::build_first_turn_prompt(history, user_text)
        };

        cmd.arg("-p").arg(&prompt);
        cmd.stdout(Stdio::piped());
        // Surface stderr (e.g. unknown flags) rather than silently swallowing it.
        cmd.stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .context("Failed to spawn `claude` — is it installed and on PATH?")?;
        let stdout = child.stdout.take().context("subprocess had no stdout")?;
        let reader = BufReader::new(stdout);

        let stream = try_stream! {
            let _child = KillOnDrop(child);
            let mut lines = reader.lines();
            while let Some(line) = lines.next_line().await? {
                if line.is_empty() {
                    continue;
                }
                let json: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue, // skip any non-JSON lines
                };
                let event_type = json["type"].as_str().unwrap_or("");
                match event_type {
                    "system" if json["subtype"].as_str() == Some("init") => {
                        // Capture the session ID so subsequent turns can use --resume.
                        if let Some(id) = json["session_id"].as_str() {
                            *session_id_arc.lock().unwrap() = Some(id.to_string());
                        }
                    }
                    "stream_event" => {
                        // Yield text deltas as they arrive for per-token UI updates.
                        if json["event"]["type"].as_str() == Some("content_block_delta")
                            && json["event"]["delta"]["type"].as_str() == Some("text_delta")
                        {
                            if let Some(text) = json["event"]["delta"]["text"].as_str() {
                                if !text.is_empty() {
                                    yield text.to_string();
                                }
                            }
                        }
                    }
                    "control_request" => {
                        let request_id = json["request_id"].as_str().unwrap_or("").to_string();
                        let tool_name = json["request"]["tool_name"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string();

                        // Build a human-readable summary from the most relevant input field.
                        let input_summary = {
                            let input = &json["request"]["input"];
                            if let Some(cmd_str) = input["command"].as_str() {
                                format!("command: {}", cmd_str)
                            } else if let Some(path) = input["path"].as_str() {
                                format!("path: {}", path)
                            } else if let Some(desc) = input["description"].as_str() {
                                desc.to_string()
                            } else {
                                serde_json::to_string(input).unwrap_or_default()
                            }
                        };

                        // Register the one-shot response channel.
                        let (resp_tx, resp_rx) = oneshot::channel::<bool>();
                        pending_permissions_arc
                            .lock()
                            .unwrap()
                            .insert(request_id.clone(), resp_tx);

                        // Forward the request to the UI via the permission channel.
                        let perm_req = PermissionRequest {
                            request_id: request_id.clone(),
                            tool_name,
                            input_summary,
                        };
                        if let Some(ref tx) = *permission_tx_arc.lock().unwrap() {
                            let _ = tx.send(perm_req);
                        }

                        // Permission dialog is wired in the UI but stdin is not
                        // currently piped — the response channel resolves but
                        // nothing is written back (permission-prompt-tool stdio
                        // is disabled to avoid startup latency).
                        let _ = resp_rx.await;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
