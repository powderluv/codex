use codex_core::config::Config;
use codex_model_provider_info::LEMONADE_OSS_PROVIDER_ID;
use std::env;
use std::io;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

#[derive(Clone)]
pub struct LemonadeClient {
    client: reqwest::Client,
    base_url: String,
}

const LEMONADE_CONNECTION_ERROR: &str = "Lemonade Server is not responding. Install it with `winget install --id AMD.LemonadeServer` and start LemonadeServer.exe.";
const LEMONADE_START_TIMEOUT_ERROR: &str =
    "Lemonade Server did not become ready within 30 seconds.";

impl LemonadeClient {
    pub async fn try_from_provider(config: &Config) -> io::Result<Self> {
        let provider = config
            .model_providers
            .get(LEMONADE_OSS_PROVIDER_ID)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Built-in provider {LEMONADE_OSS_PROVIDER_ID} not found"),
                )
            })?;
        let base_url = provider.base_url.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "oss provider must have a base_url",
            )
        })?;

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Ok(Self {
            client,
            base_url: base_url.to_string(),
        })
    }

    pub async fn ensure_server_running(&self) -> io::Result<()> {
        if self.check_server().await.is_ok() {
            return Ok(());
        }

        let server_exe = Self::find_server_exe()?;
        Self::start_server(&server_exe)?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            if self.check_server().await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        Err(io::Error::other(format!(
            "{LEMONADE_START_TIMEOUT_ERROR} {LEMONADE_CONNECTION_ERROR}"
        )))
    }

    async fn check_server(&self) -> io::Result<()> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = self.client.get(&url).send().await;

        if let Ok(resp) = response {
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "Server returned error: {} {LEMONADE_CONNECTION_ERROR}",
                    resp.status()
                )))
            }
        } else {
            Err(io::Error::other(LEMONADE_CONNECTION_ERROR))
        }
    }

    pub async fn load_model(&self, model: &str) -> io::Result<()> {
        if let Ok(cli) = Self::find_cli() {
            let status = Command::new(&cli)
                .args(["load", model])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|err| {
                    io::Error::other(format!("Failed to execute '{cli} load {model}': {err}"))
                })?;

            if status.success() {
                return Ok(());
            }
        }

        self.load_model_via_http(model).await
    }

    async fn load_model_via_http(&self, model: &str) -> io::Result<()> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let request_body = serde_json::json!({
            "model": model,
            "input": "",
            "max_output_tokens": 1
        });

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|err| io::Error::other(format!("Request failed: {err}")))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Failed to load model: {}",
                response.status()
            )))
        }
    }

    pub async fn fetch_models(&self) -> io::Result<Vec<String>> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|err| io::Error::other(format!("Request failed: {err}")))?;

        if response.status().is_success() {
            let json: serde_json::Value = response.json().await.map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("JSON parse error: {err}"),
                )
            })?;
            let models = json["data"]
                .as_array()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "No 'data' array in response")
                })?
                .iter()
                .filter_map(|model| model["id"].as_str())
                .map(std::string::ToString::to_string)
                .collect();
            Ok(models)
        } else {
            Err(io::Error::other(format!(
                "Failed to fetch models: {}",
                response.status()
            )))
        }
    }

    pub async fn download_model(&self, model: &str) -> io::Result<()> {
        let cli = Self::find_cli()?;

        let status = Command::new(&cli)
            .args(["pull", model])
            .stdout(Stdio::inherit())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| {
                io::Error::other(format!("Failed to execute '{cli} pull {model}': {err}"))
            })?;

        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Model download failed with exit code: {}",
                status.code().unwrap_or(-1)
            )))
        }
    }

    fn start_server(server_exe: &Path) -> io::Result<()> {
        Command::new(server_exe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|err| {
                io::Error::other(format!(
                    "Failed to start Lemonade Server from '{}': {err}",
                    server_exe.display()
                ))
            })
    }

    fn find_cli() -> io::Result<String> {
        if let Ok(path) = which::which("lemonade") {
            return Ok(path.to_string_lossy().to_string());
        }

        let local_app_data = env::var("LOCALAPPDATA").unwrap_or_default();
        let fallback_path = Path::new(&local_app_data)
            .join("lemonade_server")
            .join("bin")
            .join("lemonade.exe");
        if fallback_path.exists() {
            Ok(fallback_path.to_string_lossy().to_string())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Lemonade CLI not found. Install it with `winget install --id AMD.LemonadeServer`.",
            ))
        }
    }

    fn find_server_exe() -> io::Result<std::path::PathBuf> {
        if let Ok(path) = which::which("LemonadeServer.exe") {
            return Ok(path);
        }

        let local_app_data = env::var("LOCALAPPDATA").unwrap_or_default();
        let fallback_path = Path::new(&local_app_data)
            .join("lemonade_server")
            .join("bin")
            .join("LemonadeServer.exe");
        if fallback_path.exists() {
            Ok(fallback_path)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Lemonade Server executable not found. {LEMONADE_CONNECTION_ERROR}"),
            ))
        }
    }

    #[cfg(test)]
    fn from_base_url(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url: base_url.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn test_fetch_models_happy_path() {
        if std::env::var(codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
            tracing::info!(
                "{} is set; skipping test_fetch_models_happy_path",
                codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR
            );
            return;
        }

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(
                    serde_json::json!({
                        "data": [
                            {"id": "Qwen3-30B-A3B-Instruct-2507-GGUF"},
                        ]
                    })
                    .to_string(),
                    "application/json",
                ),
            )
            .mount(&server)
            .await;

        let client = LemonadeClient::from_base_url(server.uri());
        let models = client.fetch_models().await.expect("fetch models");
        assert_eq!(models, vec!["Qwen3-30B-A3B-Instruct-2507-GGUF".to_string()]);
    }

    #[tokio::test]
    async fn test_fetch_models_no_data_array() {
        if std::env::var(codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
            tracing::info!(
                "{} is set; skipping test_fetch_models_no_data_array",
                codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR
            );
            return;
        }

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/models"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(serde_json::json!({}).to_string(), "application/json"),
            )
            .mount(&server)
            .await;

        let client = LemonadeClient::from_base_url(server.uri());
        let result = client.fetch_models().await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No 'data' array in response")
        );
    }
}
