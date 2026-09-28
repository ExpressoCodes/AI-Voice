use std::path::PathBuf;

use anyhow::{bail, Context};
use tokio::io::AsyncWriteExt;
use tracing::info;

const SILERO_VAD_URL: &str =
    "https://huggingface.co/onnx-community/silero-vad/resolve/main/onnx/model.onnx";

pub fn silero_vad_model_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("voicechat")
        .join("models")
        .join("silero_vad.onnx")
}

/// Ensure the Silero VAD ONNX model is present on disk, downloading if needed.
/// Returns the path to the model file.
pub async fn ensure_model() -> anyhow::Result<PathBuf> {
    let model_path = silero_vad_model_path();

    if model_path.exists() {
        info!(
            "Silero VAD model already present at {}",
            model_path.display()
        );
        return Ok(model_path);
    }

    info!("Silero VAD model not found. Downloading...");

    if let Some(parent) = model_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create model directory")?;
    }

    let client = reqwest::Client::new();
    let response = client
        .get(SILERO_VAD_URL)
        .send()
        .await
        .context("Failed to start Silero VAD model download")?;

    if !response.status().is_success() {
        bail!(
            "Silero VAD model download failed with status: {}",
            response.status()
        );
    }

    // Write to a temp file first, then rename atomically.
    let temp_path = model_path.with_extension("onnx.tmp");
    {
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .context("Failed to create temp download file")?;

        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("Download stream error")?;
            file.write_all(&bytes)
                .await
                .context("Failed to write download chunk")?;
        }
        file.flush().await?;
    }

    info!(
        "Download complete. Moving model to {}",
        model_path.display()
    );
    tokio::fs::rename(&temp_path, &model_path)
        .await
        .context("Failed to move Silero VAD model into place")?;

    Ok(model_path)
}
