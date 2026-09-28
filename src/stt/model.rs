use std::path::PathBuf;

use anyhow::{bail, Context};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::config;

/// Ensure the Whisper model is present on disk, downloading if needed.
/// Returns the path to the model file.
pub async fn ensure_model() -> anyhow::Result<PathBuf> {
    let model_path = config::whisper_model_path();

    if model_path.exists() {
        info!("Whisper model already present at {}", model_path.display());
        return Ok(model_path);
    }

    info!("Whisper model not found. Downloading...");

    // Create parent directories
    if let Some(parent) = model_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create model directory")?;
    }

    let url = config::whisper_model_url();
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .context("Failed to start model download")?;

    if !response.status().is_success() {
        bail!(
            "Model download failed with status: {}",
            response.status()
        );
    }

    // Write to a temp file first, then move
    let temp_path = model_path.with_extension("bin.tmp");
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

    info!("Download complete. Moving model to {}", model_path.display());
    tokio::fs::rename(&temp_path, &model_path)
        .await
        .context("Failed to move model into place")?;

    Ok(model_path)
}
