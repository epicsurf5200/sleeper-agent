use crate::strategy::Strategy;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnthropicConfig {
    /// API key. Falls back to ANTHROPIC_API_KEY env var when empty.
    #[serde(default)]
    pub api_key: String,
    /// Backend: "auto" (default), "api", or "claude-cli".
    /// auto → API when a key is set, otherwise the `claude` CLI (subscription auth).
    #[serde(default)]
    pub backend: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Per-feature backend overrides. Each is "" (inherit `backend`), "api",
    /// or "claude-cli" — so the fast/metered API can drive the interactive
    /// features while the background daemon stays on the subscription CLI,
    /// or vice versa.
    #[serde(default)]
    pub features: FeatureBackends,
    /// Extended-thinking budget for the `claude-cli` backend, in tokens.
    ///
    /// The CLI enables extended thinking by default, which for these prompts
    /// spent ~75% of every response on discarded thinking tokens and made a
    /// lineup call take 14-30s instead of ~7s. Analysis here is short and
    /// well-structured, so the default is 0 (off). Raise it if you want the
    /// model to deliberate harder at the cost of latency.
    #[serde(default)]
    pub thinking_tokens: u32,
}

/// Which backend each AI-backed feature uses. Empty string inherits
/// `anthropic.backend`; otherwise "api" or "claude-cli".
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureBackends {
    #[serde(default)]
    pub lineup: String,
    #[serde(default)]
    pub waiver: String,
    #[serde(default)]
    pub trade: String,
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub trending: String,
    #[serde(default)]
    pub daemon: String,
}

fn default_model() -> String {
    "claude-sonnet-4-6".into()
}

fn default_max_tokens() -> u32 {
    2048
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SleeperConfig {
    /// Sleeper username or numeric user_id. Required.
    pub username: String,
    /// Optional — auto-discovered from your leagues when empty.
    #[serde(default)]
    pub league_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub strategy: Strategy,
    #[serde(default = "default_refresh")]
    pub refresh_seconds: u64,
    #[serde(default = "default_news")]
    pub news_sources: Vec<String>,
    /// Extra context files (.md/.txt — league special rules, keeper notes,
    /// personal preferences, …) injected into every AI prompt.
    /// Relative paths resolve against the config file's directory.
    #[serde(default)]
    pub context_files: Vec<String>,
}

/// Which events the background daemon is allowed to alert on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Triggers {
    /// AI lineup differs from the lineup currently set in Sleeper.
    #[serde(default = "yes")]
    pub better_lineup: bool,
    /// A player you are currently starting is Out/Doubtful/IR/Suspended.
    #[serde(default = "yes")]
    pub injured_starter: bool,
    /// A free agent would upgrade a weak roster spot.
    #[serde(default = "yes")]
    pub waiver: bool,
    /// AI-generated trade ideas against other rosters.
    #[serde(default = "yes")]
    pub trade: bool,
}

fn yes() -> bool {
    true
}

impl Default for Triggers {
    fn default() -> Self {
        Self { better_lineup: true, injured_starter: true, waiver: true, trade: true }
    }
}

/// Outbound webhook. Discord is the default shape; `format: json` posts the
/// raw alert instead, for Home Assistant / n8n / anything custom.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotifyConfig {
    /// Webhook URL. Empty disables notifications entirely.
    /// Falls back to the SA_WEBHOOK_URL env var when empty — keeps the
    /// secret out of config.yaml on a shared server.
    #[serde(default)]
    pub webhook_url: String,
    /// "discord" (default) or "json".
    #[serde(default)]
    pub format: String,
}

impl NotifyConfig {
    pub fn is_enabled(&self) -> bool {
        !self.webhook_url.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// How often to run a full AI analysis pass.
    #[serde(default = "default_interval")]
    pub interval_minutes: u64,
    /// Skip analysis outside this local-hour window (inclusive start, exclusive
    /// end). Defaults to 8am–11pm so the server does not buzz you at 4am.
    #[serde(default = "default_quiet_start")]
    pub active_hour_start: u32,
    #[serde(default = "default_quiet_end")]
    pub active_hour_end: u32,
    #[serde(default)]
    pub triggers: Triggers,
}

fn default_interval() -> u64 {
    180
}

fn default_quiet_start() -> u32 {
    8
}

fn default_quiet_end() -> u32 {
    23
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            interval_minutes: default_interval(),
            active_hour_start: default_quiet_start(),
            active_hour_end: default_quiet_end(),
            triggers: Triggers::default(),
        }
    }
}

fn default_refresh() -> u64 {
    900
}

fn default_news() -> Vec<String> {
    vec![
        "https://www.espn.com/espn/rss/nfl/news".into(),
        "https://profootballtalk.nbcsports.com/feed/".into(),
        "https://www.cbssports.com/rss/headlines/nfl/".into(),
    ]
}

/// Feeds that used to ship as defaults but now 404. Dropped on load so an
/// existing config.yaml stops logging a warning every cycle.
const DEAD_NEWS_SOURCES: &[&str] = &["https://api.sleeper.app/news/nfl/rss"];

impl Default for Settings {
    fn default() -> Self {
        Self {
            strategy: Strategy::default(),
            refresh_seconds: default_refresh(),
            news_sources: default_news(),
            context_files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub anthropic: AnthropicConfig,
    pub sleeper: SleeperConfig,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Directory the config was loaded from — anchors relative context_files.
    #[serde(skip)]
    pub base_dir: PathBuf,
    /// Path the config was loaded from — so the settings UI can save back.
    #[serde(skip)]
    pub path: PathBuf,
    /// Secrets that came from the environment rather than the file. Tracked so
    /// `save` never writes them into config.yaml.
    #[serde(skip)]
    pub api_key_from_env: bool,
    #[serde(skip)]
    pub webhook_from_env: bool,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let mut cfg: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("parsing config at {}", path.display()))?;
        if cfg.anthropic.api_key.is_empty() {
            if let Ok(env) = std::env::var("ANTHROPIC_API_KEY") {
                cfg.anthropic.api_key = env;
                cfg.api_key_from_env = true;
            }
        }
        if cfg.notify.webhook_url.is_empty() {
            if let Ok(env) = std::env::var("SA_WEBHOOK_URL") {
                cfg.notify.webhook_url = env;
                cfg.webhook_from_env = true;
            }
        }
        let before = cfg.settings.news_sources.len();
        cfg.settings
            .news_sources
            .retain(|s| !DEAD_NEWS_SOURCES.contains(&s.as_str()));
        if cfg.settings.news_sources.len() != before {
            tracing::debug!("dropped retired news source(s) from config");
        }
        if cfg.settings.news_sources.is_empty() {
            cfg.settings.news_sources = default_news();
        }
        cfg.path = path.to_path_buf();
        cfg.base_dir = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Ok(cfg)
    }

    /// Read `settings.context_files` and bundle them into one block for AI
    /// prompts. Returns None when no files are configured; errors on a
    /// missing/unreadable file so typos don't get silently ignored.
    pub fn load_context(&self) -> Result<Option<String>> {
        if self.settings.context_files.is_empty() {
            return Ok(None);
        }
        let mut out = String::from(
            "ADDITIONAL LEAGUE CONTEXT (user-provided; treat as authoritative \
             for league rules, scoring quirks, and preferences):\n",
        );
        for entry in &self.settings.context_files {
            let p = Path::new(entry);
            let resolved = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.base_dir.join(p)
            };
            let text = std::fs::read_to_string(&resolved)
                .with_context(|| format!("reading context file {}", resolved.display()))?;
            out.push_str(&format!("\n--- {} ---\n{}\n", entry, text.trim()));
        }
        Ok(Some(out))
    }

    /// Write the config back to the file it came from. Secrets sourced from the
    /// environment are stripped first so they are never persisted to disk, and
    /// the file is written 0600 since it can hold an API key.
    pub fn save(&self) -> Result<()> {
        let mut out = self.clone();
        if out.api_key_from_env {
            out.anthropic.api_key.clear();
        }
        if out.webhook_from_env {
            out.notify.webhook_url.clear();
        }
        let yaml = serde_yaml::to_string(&out).context("serializing config")?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        std::fs::write(&self.path, yaml)
            .with_context(|| format!("writing config to {}", self.path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Defaults for the iOS app.
    ///
    /// iOS cannot spawn subprocesses, so the Claude CLI backend is
    /// unreachable there. Pinning `backend` to "api" makes that explicit:
    /// without a key the app reports a missing key, rather than "auto"
    /// resolving to a CLI that can never run.
    pub fn for_ios() -> Self {
        let mut c = Self {
            anthropic: AnthropicConfig {
                backend: "api".into(),
                model: default_model(),
                max_tokens: default_max_tokens(),
                ..Default::default()
            },
            sleeper: SleeperConfig::default(),
            settings: Settings::default(),
            notify: NotifyConfig::default(),
            daemon: DaemonConfig::default(),
            base_dir: PathBuf::new(),
            path: PathBuf::new(),
            api_key_from_env: false,
            webhook_from_env: false,
        };
        // Context files are desktop paths; the phone has no equivalent.
        c.settings.context_files.clear();
        c
    }

    pub fn default_path() -> PathBuf {
        if let Ok(custom) = std::env::var("SA_CONFIG") {
            return PathBuf::from(custom);
        }
        let local = PathBuf::from("config.yaml");
        if local.exists() {
            return local;
        }
        if let Some(dir) = dirs::config_dir() {
            return dir.join("sleeper-agent").join("config.yaml");
        }
        local
    }
}
