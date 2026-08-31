use crate::config::AnthropicConfig;
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Messages endpoint — overridable via ANTHROPIC_BASE_URL for tests/mocks.
fn api_url() -> String {
    match std::env::var("ANTHROPIC_BASE_URL") {
        Ok(base) => format!("{}/v1/messages", base.trim_end_matches('/')),
        Err(_) => API_URL.to_string(),
    }
}

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
pub enum Backend {
    /// Direct Messages API call with an API key.
    Api,
    /// Shell out to the Claude Code CLI (`claude -p`) — uses the user's
    /// Pro/Max subscription login instead of an API key.
    ClaudeCli,
}

/// The AI-backed features, each of which can be pinned to its own backend.
/// Interactive work often wants the fast metered API while the background
/// daemon stays on the subscription CLI (or the reverse) — see
/// `anthropic.features` in config.yaml.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AiFeature {
    Lineup,
    Waiver,
    Trade,
    Draft,
    Trending,
    Daemon,
}

impl AiFeature {
    pub const ALL: [AiFeature; 6] = [
        Self::Lineup,
        Self::Waiver,
        Self::Trade,
        Self::Draft,
        Self::Trending,
        Self::Daemon,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Lineup => "Lineup",
            Self::Waiver => "Waiver",
            Self::Trade => "Trade",
            Self::Draft => "Draft",
            Self::Trending => "Trending",
            Self::Daemon => "Daemon",
        }
    }

    /// The matching field on `FeatureBackends`.
    fn spec<'a>(&self, f: &'a crate::config::FeatureBackends) -> &'a str {
        match self {
            Self::Lineup => &f.lineup,
            Self::Waiver => &f.waiver,
            Self::Trade => &f.trade,
            Self::Draft => &f.draft,
            Self::Trending => &f.trending,
            Self::Daemon => &f.daemon,
        }
    }
}

/// Resolve a backend spec ("", "auto", "api", "claude-cli") against the
/// credentials actually available.
fn resolve_backend(spec: &str, cfg: &AnthropicConfig) -> Result<Backend> {
    match spec.trim() {
        "api" => {
            if cfg.api_key.trim().is_empty() {
                return Err(anyhow!(
                    "backend 'api' requested but no API key is set (anthropic.api_key or ANTHROPIC_API_KEY)"
                ));
            }
            Ok(Backend::Api)
        }
        "claude-cli" => {
            if !claude_cli_available() {
                return Err(anyhow!(
                    "backend 'claude-cli' requested but the `claude` CLI was not found on PATH — install Claude Code and log in"
                ));
            }
            Ok(Backend::ClaudeCli)
        }
        "" | "auto" => {
            if !cfg.api_key.trim().is_empty() {
                Ok(Backend::Api)
            } else if claude_cli_available() {
                Ok(Backend::ClaudeCli)
            } else {
                Err(anyhow!(
                    "no Anthropic credentials: set anthropic.api_key / ANTHROPIC_API_KEY, or install the Claude Code CLI to use your Claude subscription"
                ))
            }
        }
        other => Err(anyhow!(
            "unknown backend '{}' (expected auto | api | claude-cli)",
            other
        )),
    }
}

#[derive(Clone)]
pub struct Anthropic {
    http: Client,
    cfg: AnthropicConfig,
    backend: Backend,
    /// Resolved per-feature overrides. Absent means "use `backend`".
    feature_backends: std::collections::HashMap<AiFeature, Backend>,
    /// Neutral working directory for the `claude` child process.
    cli_workdir: std::path::PathBuf,
    /// When set, every completion uses this backend regardless of feature.
    /// The daemon pins itself this way: it calls the lineup/waiver/trade code
    /// paths, but the operator configures the daemon as a single unit.
    forced: Option<Backend>,
    /// User-provided context (league rules, notes, …) appended to every
    /// system prompt. See `Config::load_context`.
    context: Option<String>,
}

impl Anthropic {
    pub fn new(cfg: AnthropicConfig) -> Result<Self> {
        let backend = resolve_backend(&cfg.backend, &cfg)
            .with_context(|| "resolving anthropic.backend".to_string())?;

        // A per-feature override that cannot be honoured (say "api" with no
        // key) falls back to the working default with a warning rather than
        // taking the whole app down — the feature still runs.
        let mut feature_backends = std::collections::HashMap::new();
        for feat in AiFeature::ALL {
            let spec = feat.spec(&cfg.features);
            if spec.trim().is_empty() {
                continue;
            }
            match resolve_backend(spec, &cfg) {
                Ok(b) => {
                    feature_backends.insert(feat, b);
                }
                Err(e) => tracing::warn!(
                    feature = feat.label(),
                    error = %e,
                    "per-feature backend unavailable; falling back to the default backend"
                ),
            }
        }

        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("groks_fantasy/0.2 (anthropic)")
            .build()?;
        // The child inherits our working directory unless told otherwise, and
        // Claude Code treats that directory as a project: it reads the files
        // and git state around it and folds them into the prompt. Launched
        // from Finder that directory is `/`; launched from a terminal it is
        // whatever the user happened to be sitting in. Neither belongs in a
        // fantasy-football prompt, and on macOS the reads are attributed to
        // this app, which is what triggers the Documents/Downloads/Photos
        // permission prompts. Point it at an empty directory we own instead.
        let cli_workdir = match std::env::var("SA_CACHE_DIR") {
            Ok(dir) => std::path::PathBuf::from(dir),
            Err(_) => dirs::cache_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("sleeper-agent"),
        }
        .join("cli-workdir");
        let _ = std::fs::create_dir_all(&cli_workdir);

        Ok(Self {
            http,
            cfg,
            backend,
            feature_backends,
            cli_workdir,
            forced: None,
            context: None,
        })
    }

    /// Clone pinned to one feature's backend, so every completion made
    /// through it uses that backend whichever code path it goes down.
    pub fn pinned_to(&self, feature: AiFeature) -> Self {
        let mut out = self.clone();
        out.forced = Some(self.backend_for(feature));
        out
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

    /// Completion routed through whichever backend this feature is pinned to.
    pub async fn complete_for(
        &self,
        feature: AiFeature,
        system: &str,
        user: &str,
    ) -> Result<String> {
        self.dispatch(self.backend_for(feature), system, user, None).await
    }

    /// The backend a feature will actually use, after overrides.
    pub fn backend_for(&self, feature: AiFeature) -> Backend {
        if let Some(b) = self.forced {
            return b;
        }
        self.feature_backends.get(&feature).copied().unwrap_or(self.backend)
    }

    /// Human-readable backend name for a feature, for the settings UI.
    pub fn backend_name(&self, feature: AiFeature) -> &'static str {
        match self.backend_for(feature) {
            Backend::Api => "api",
            Backend::ClaudeCli => "claude-cli",
        }
    }

    pub async fn complete_with(
        &self,
        system: &str,
        user: &str,
        temperature: Option<f32>,
    ) -> Result<String> {
        self.dispatch(self.backend, system, user, temperature).await
    }

    async fn dispatch(
        &self,
        backend: Backend,
        system: &str,
        user: &str,
        temperature: Option<f32>,
    ) -> Result<String> {
        let started = std::time::Instant::now();
        let out = match backend {
            Backend::Api => self.complete_api(system, user, temperature).await,
            // The CLI has no temperature knob; ignore it there.
            Backend::ClaudeCli => self.complete_cli(system, user).await,
        };
        // Latency is the dominant cost of every AI-backed command, so make it
        // measurable rather than something you have to time with a stopwatch.
        tracing::info!(
            backend = ?backend,
            model = %self.cfg.model,
            prompt_chars = system.len() + user.len(),
            reply_chars = out.as_ref().map(|s| s.len()).unwrap_or(0),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "ai completion"
        );
        out
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
            .post(api_url())
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
            // Don't inherit the user's Claude Code environment: no MCP servers,
            // no user/project/local settings, no CLAUDE.md, skills or plugins.
            // This app wants a plain text completion, and every one of those
            // is extra file access performed under this app's identity.
            .arg("--strict-mcp-config")
            .arg("--setting-sources")
            .arg("")
            .current_dir(&self.cli_workdir)
            // The CLI turns extended thinking on by default. These prompts are
            // short and the answer format is fixed, so the thinking tokens are
            // pure latency — measured 14-30s per call with it on versus ~7s
            // with it off. `anthropic.thinking_tokens` raises it back if you
            // want deliberation.
            .env("MAX_THINKING_TOKENS", self.cfg.thinking_tokens.to_string())
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
