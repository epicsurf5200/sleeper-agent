//! Standalone desktop launcher: `sa-gui`. Equivalent to `sa gui`.

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
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
    let strategy = cfg.settings.strategy;
    tokio::task::block_in_place(move || gui::run(rt, session, anthropic, scheduler, strategy))
}
