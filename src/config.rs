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

fn default_refresh() -> u64 {
    900
}

fn default_news() -> Vec<String> {
    vec![
        "https://www.espn.com/espn/rss/nfl/news".into(),
        "https://api.sleeper.app/news/nfl/rss".into(),
    ]
}

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
    /// Directory the config was loaded from — anchors relative context_files.
    #[serde(skip)]
    pub base_dir: PathBuf,
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
            }
        }
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
