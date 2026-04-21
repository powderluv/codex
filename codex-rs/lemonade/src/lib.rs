mod client;

pub use client::LemonadeClient;
use codex_core::config::Config;

/// Default OSS model to use when `--oss` is passed without an explicit `-m`.
pub const DEFAULT_OSS_MODEL: &str = "Qwen3-30B-A3B-Instruct-2507-GGUF";

/// Prepare the local OSS environment when `--oss` is selected.
///
/// - Ensures a local Lemonade server is reachable, starting it when possible.
/// - Checks if the model exists locally and downloads it if missing.
/// - Pre-warms the model so the first turn does not pay the full cold-load cost.
pub async fn ensure_oss_ready(config: &Config) -> std::io::Result<()> {
    let model = match config.model.as_ref() {
        Some(model) => model,
        None => DEFAULT_OSS_MODEL,
    };

    let lemonade_client = LemonadeClient::try_from_provider(config).await?;
    lemonade_client.ensure_server_running().await?;

    match lemonade_client.fetch_models().await {
        Ok(models) => {
            if !models.iter().any(|m| m == model) {
                lemonade_client.download_model(model).await?;
            }
        }
        Err(err) => {
            tracing::warn!("Failed to query local models from Lemonade: {}.", err);
        }
    }

    tokio::spawn({
        let client = lemonade_client.clone();
        let model = model.to_string();
        async move {
            if let Err(err) = client.load_model(&model).await {
                tracing::warn!("Failed to load model {} from Lemonade: {}", model, err);
            }
        }
    });

    Ok(())
}
