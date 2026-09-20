//! Standalone desktop launcher: `sa-gui`. Equivalent to `sa gui`.
//!
//! On Windows this is a GUI-subsystem executable: no console window is
//! opened alongside the app. Because that leaves no stderr to write to,
//! logs go to `<cache dir>/sleeper-agent/sa-gui.log` and a fatal startup
//! error is shown in a native message box instead of vanishing silently.
#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{Context, Result};
use clap::Parser;
use sleeper_agent::api::{LeagueSession, SleeperClient};
use sleeper_agent::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "sa-gui", version, about = "Desktop GUI for sleeper-agent")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(short, long)]
    strategy: Option<String>,
    #[arg(short, long)]
    league: Option<String>,
}

fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

/// Where `sa-gui.log` lives when there is no console to log to.
fn log_path() -> PathBuf {
    let dir = match std::env::var("SA_CACHE_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("sleeper-agent"),
    };
    dir.join("sa-gui.log")
}

/// Windows (no console): log to a file, falling back to stderr if the file
/// can't be opened. Elsewhere: stderr, same as `sa`.
fn init_logging() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let path = log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
            return Some(path);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .init();
    None
}

#[cfg(windows)]
fn fatal_dialog(text: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            utype: u32,
        ) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide("sleeper-agent"));
    const MB_ICONERROR: u32 = 0x10;
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR) };
}

#[cfg(not(windows))]
fn fatal_dialog(_text: &str) {}

#[tokio::main]
async fn main() -> Result<()> {
    let log_file = init_logging();
    match run().await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!("sa-gui failed: {e:#}");
            let mut msg = format!("sleeper-agent could not start:

{e:#}");
            if let Some(p) = &log_file {
                msg.push_str(&format!("

Log: {}", p.display()));
            }
            fatal_dialog(&msg);
            Err(e)
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg_path = cli.config.unwrap_or_else(config::Config::default_path);
    let mut cfg = config::Config::load(&cfg_path)
        .with_context(|| format!("loading config {}", cfg_path.display()))?;
    if let Some(s) = &cli.strategy {
        cfg.settings.strategy = s.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    }
    let client = Arc::new(SleeperClient::new()?);
    let league_override = cli.league.as_deref().or({
        let id = cfg.sleeper.league_id.as_str();
        if id.is_empty() { None } else { Some(id) }
    });
    let session = Arc::new(
        LeagueSession::connect(client, &cfg.sleeper.username, league_override).await?,
    );
    let anthropic =
        anthropic::Anthropic::new(cfg.anthropic.clone())?.with_context(cfg.load_context()?);
    let news_fetcher = Arc::new(news::NewsFetcher::new(cfg.settings.news_sources.clone())?);
    let scheduler = Arc::new(scheduler::Scheduler::new(Duration::from_secs(
        cfg.settings.refresh_seconds,
    )));
    scheduler.spawn(session.clone(), news_fetcher.clone());
    let rt = tokio::runtime::Handle::current();
    tokio::task::block_in_place(move || gui::run(rt, session, anthropic, scheduler, cfg))
}
