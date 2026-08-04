use crate::config::AnthropicConfig;
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<Message<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    kind: String,
}

/// Which route AI completions take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    /// Direct Messages API call with an API key.
    Api,
    /// Shell out to the Claude Code CLI (`claude -p`) — uses the user's
    /// Pro/Max subscription login instead of an API key.
    ClaudeCli,
}

#[derive(Clone)]
pub struct Anthropic {
    http: Client,
    cfg: AnthropicConfig,
    backend: Backend,
    /// User-provided context (league rules, notes, …) appended to every
    /// system prompt. See `Config::load_context`.
    context: Option<String>,
}

impl Anthropic {
    pub fn new(cfg: AnthropicConfig) -> Result<Self> {
        let backend = match cfg.backend.trim() {
            "api" => {
                if cfg.api_key.trim().is_empty() {
                    return Err(anyhow!(
                        "anthropic.backend is 'api' but no API key is set (anthropic.api_key or ANTHROPIC_API_KEY)"
                    ));
                }
                Backend::Api
            }
            "claude-cli" => {
                if !claude_cli_available() {
                    return Err(anyhow!(
                        "anthropic.backend is 'claude-cli' but the `claude` CLI was not found on PATH — install Claude Code and log in"
                    ));
                }
                Backend::ClaudeCli
            }
            "" | "auto" => {
                if !cfg.api_key.trim().is_empty() {
                    Backend::Api
                } else if claude_cli_available() {
                    Backend::ClaudeCli
                } else {
                    return Err(anyhow!(
                        "no Anthropic credentials: set anthropic.api_key / ANTHROPIC_API_KEY, or install the Claude Code CLI to use your Claude subscription"
                    ));
                }
            }
            other => {
                return Err(anyhow!(
                    "unknown anthropic.backend '{}' (expected auto | api | claude-cli)",
                    other
                ));
            }
        };
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("groks_fantasy/0.2 (anthropic)")
            .build()?;
        Ok(Self {
            http,
            cfg,
            backend,
            context: None,
        })
    }

    /// Attach user-provided context appended to every system prompt.
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// System prompt with any user context appended.
    fn effective_system(&self, system: &str) -> String {
        match &self.context {
            Some(ctx) => format!("{system}\n\n{ctx}"),
            None => system.to_string(),
        }
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub async fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.complete_with(system, user, None).await
    }

    pub async fn complete_with(
        &self,
        system: &str,
        user: &str,
        temperature: Option<f32>,
    ) -> Result<String> {
        match self.backend {
            Backend::Api => self.complete_api(system, user, temperature).await,
            // The CLI has no temperature knob; ignore it there.
            Backend::ClaudeCli => self.complete_cli(system, user).await,
        }
    }

    async fn complete_api(
        &self,
        system: &str,
        user: &str,
        temperature: Option<f32>,
    ) -> Result<String> {
        let system = self.effective_system(system);
        let body = MessagesRequest {
            model: &self.cfg.model,
            max_tokens: self.cfg.max_tokens,
            system: &system,
            messages: vec![Message {
                role: "user",
                content: user,
            }],
            temperature,
        };

        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.cfg.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("anthropic request failed")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(anyhow!(
                "anthropic API returned {} — {}",
                status,
                truncate(&text, 500)
            ));
        }

        let parsed: MessagesResponse = serde_json::from_str(&text)
            .with_context(|| format!("decoding anthropic response: {}", truncate(&text, 200)))?;

        if let Some(err) = parsed.error {
            return Err(anyhow!("anthropic error [{}]: {}", err.kind, err.message));
        }
        if parsed.stop_reason.as_deref() == Some("max_tokens") {
            tracing::warn!(
                max_tokens = self.cfg.max_tokens,
                "anthropic response truncated at max_tokens — consider raising anthropic.max_tokens"
            );
        }

        let mut out = String::new();
        for block in parsed.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(&text);
            }
        }
        if out.is_empty() {
            return Err(anyhow!("empty response from Anthropic"));
        }
        Ok(out)
    }

    /// Headless completion via the Claude Code CLI (`claude -p`).
    /// Authenticates with the user's Claude subscription login; `--tools ""`
    /// disables all agent tools so this behaves as a pure text completion.
    async fn complete_cli(&self, system: &str, user: &str) -> Result<String> {
        let mut child = tokio::process::Command::new("claude")
            .arg("-p")
            .arg("--output-format")
            .arg("text")
            .arg("--model")
            .arg(&self.cfg.model)
            .arg("--system-prompt")
            .arg(self.effective_system(system))
            .arg("--tools")
            .arg("")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // On timeout the future is dropped — make sure the child dies
            // with it instead of lingering as an orphan.
            .kill_on_drop(true)
            .spawn()
            .context("spawning `claude` CLI — is Claude Code installed?")?;

        // Prompt goes over stdin so long/`-`-prefixed prompts can't break argv.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open stdin for `claude` CLI"))?;
        stdin
            .write_all(user.as_bytes())
            .await
            .context("writing prompt to `claude` CLI")?;
        drop(stdin);

        let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
            .await
            .map_err(|_| anyhow!("`claude` CLI timed out after 300s"))?
            .context("waiting for `claude` CLI")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = if stderr.trim().is_empty() { stdout } else { stderr };
            return Err(anyhow!(
                "`claude` CLI exited with {} — {}",
                output.status,
                truncate(detail.trim(), 500)
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(anyhow!("empty response from `claude` CLI"));
        }
        Ok(text)
    }
}

/// True when a `claude` executable is on PATH.
fn claude_cli_available() -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join("claude").is_file())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        // Take by chars, not bytes — slicing at a byte offset can panic
        // mid-way through a multi-byte character.
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
