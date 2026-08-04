use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sleeper_agent::api::{LeagueSession, SleeperClient};
use sleeper_agent::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "sa", version, about = "sleeper-agent — autonomous Claude-powered manager for Sleeper leagues")]
struct Cli {
    /// Path to config.yaml (defaults to ./config.yaml or $XDG_CONFIG_HOME/sleeper-agent/config.yaml).
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Override strategy: conservative | balanced | high_stakes.
    #[arg(short, long, global = true)]
    strategy: Option<String>,

    /// Override league_id for this run (otherwise config/auto-discovery).
    #[arg(short, long, global = true)]
    league: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Launch the terminal UI (default).
    Ui,
    /// Launch the desktop GUI.
    Gui,
    /// List every Sleeper league your account is in this season.
    Leagues,
    /// Print your roster (with real Sleeper projections).
    Roster,
    /// Generate an AI-recommended lineup.
    Lineup {
        #[arg(short, long)]
        week: Option<u8>,
    },
    /// Waiver suggestions: projections + trending adds + league transactions.
    Waiver {
        #[arg(short, long, default_value_t = 300)]
        pool: usize,
    },
    /// Analyze a proposed trade.
    Trade {
        #[arg(short, long)]
        partner: String,
        #[arg(short, long)]
        send: String,
        #[arg(short, long)]
        receive: String,
    },
    /// Backtest the AI manager against a real historical season.
    Backtest {
        /// Season to replay (e.g. 2025).
        #[arg(long, default_value = "2025")]
        season: String,
        /// Number of weeks to replay.
        #[arg(long, default_value_t = 14)]
        weeks: u8,
        /// Draft slot (1-12) for the backtested team.
        #[arg(long, default_value_t = 5)]
        slot: usize,
        /// Override the Claude model for this run (e.g. claude-opus-4-8).
        #[arg(long)]
        model: Option<String>,
        /// Skip AI calls; sweep the form-blend weight to size the headroom.
        #[arg(long)]
        dry: bool,
    },
    /// Show league-wide trending adds and drops (last 24h).
    Trending,
    /// Show recent league transactions (waivers, trades, FA moves).
    Transactions {
        #[arg(short, long, default_value_t = 3)]
        weeks: u8,
    },
    /// Show playoff winners/losers brackets.
    Bracket,
    /// Show future draft picks that changed hands.
    TradedPicks,
    /// Watch the draft; prints AI suggestions when you're on the clock.
    Draft {
        #[arg(short = 'i', long, default_value_t = 10)]
        interval: u64,
    },
    /// One-shot AI draft suggestion for the next pick.
    DraftSuggest,
    /// Print league summary.
    Info,
    /// Write a starter config.yaml in the current directory.
    Init,
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

    if let Some(Command::Init) = &cli.command {
        return write_starter_config();
    }

    let cfg_path = cli.config.clone().unwrap_or_else(config::Config::default_path);
    let mut cfg = config::Config::load(&cfg_path)
        .with_context(|| format!("loading config {}", cfg_path.display()))?;
    if let Some(s) = &cli.strategy {
        cfg.settings.strategy = s.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    }

    let client = Arc::new(SleeperClient::new()?);

    // `backtest` needs no league — handle before full connect.
    if let Some(Command::Backtest { season, weeks, slot, model, dry }) = &cli.command {
        let mut acfg = cfg.anthropic.clone();
        if let Some(m) = model {
            acfg.model = m.clone();
        }
        let anthropic = anthropic::Anthropic::new(acfg)?.with_context(cfg.load_context()?);
        return backtest::run(
            &client,
            &anthropic,
            backtest::BacktestArgs {
                season: season.clone(),
                weeks: *weeks,
                slot: *slot,
                strategy: cfg.settings.strategy,
                dry: *dry,
            },
        )
        .await;
    }

    // `leagues` only needs the username — handle before full connect.
    if let Some(Command::Leagues) = &cli.command {
        let leagues = LeagueSession::discover_leagues(&client, &cfg.sleeper.username).await?;
        println!("Leagues for '{}':", cfg.sleeper.username);
        for l in leagues {
            println!(
                "  {}  {:<32} {} teams  {}  ({})",
                l.league_id, l.name, l.total_rosters, l.scoring, l.season
            );
        }
        println!("\nPin one with `sleeper.league_id` in config.yaml or --league <id>.");
        return Ok(());
    }

    let league_override = cli.league.as_deref().or({
        let id = cfg.sleeper.league_id.as_str();
        if id.is_empty() { None } else { Some(id) }
    });
    let session = Arc::new(
        LeagueSession::connect(client.clone(), &cfg.sleeper.username, league_override).await?,
    );
    // Only AI-backed commands need the Anthropic key — construct lazily.
    let anthropic = || -> Result<anthropic::Anthropic> {
        Ok(anthropic::Anthropic::new(cfg.anthropic.clone())?.with_context(cfg.load_context()?))
    };
    let news_fetcher = Arc::new(news::NewsFetcher::new(cfg.settings.news_sources.clone())?);

    match cli.command.unwrap_or(Command::Ui) {
        Command::Ui => {
            ui::App::new(
                session,
                anthropic()?,
                news_fetcher,
                cfg.settings.strategy,
                Duration::from_secs(cfg.settings.refresh_seconds),
            )
            .run()
            .await
        }
        Command::Gui => run_gui(session, anthropic()?, news_fetcher, cfg).await,
        Command::Roster => cmd_roster(&session).await,
        Command::Info => cmd_info(&session).await,
        Command::Lineup { week } => cmd_lineup(&session, &anthropic()?, &news_fetcher, &cfg, week).await,
        Command::Waiver { pool } => cmd_waiver(&session, &anthropic()?, &news_fetcher, &cfg, pool).await,
        Command::Trade { partner, send, receive } => {
            cmd_trade(&session, &anthropic()?, &news_fetcher, &cfg, partner, send, receive).await
        }
        Command::Trending => cmd_trending(&session).await,
        Command::Transactions { weeks } => cmd_transactions(&session, weeks).await,
        Command::Bracket => cmd_bracket(&session).await,
        Command::TradedPicks => cmd_traded_picks(&session).await,
        Command::Draft { interval } => cmd_draft_watch(&session, &anthropic()?, &news_fetcher, &cfg, interval).await,
        Command::DraftSuggest => cmd_draft_suggest(&session, &anthropic()?, &news_fetcher, &cfg).await,
        Command::Leagues | Command::Init | Command::Backtest { .. } => unreachable!(),
    }
}

#[cfg(feature = "gui")]
async fn run_gui(
    session: Arc<LeagueSession>,
    anthropic: anthropic::Anthropic,
    news_fetcher: Arc<news::NewsFetcher>,
    cfg: config::Config,
) -> Result<()> {
    let scheduler = Arc::new(scheduler::Scheduler::new(Duration::from_secs(
        cfg.settings.refresh_seconds,
    )));
    scheduler.spawn(session.clone(), news_fetcher.clone());
    let rt = tokio::runtime::Handle::current();
    let strategy = cfg.settings.strategy;
    tokio::task::block_in_place(move || {
        gui::run(rt, session, anthropic, scheduler, strategy)
    })
}

#[cfg(not(feature = "gui"))]
async fn run_gui(
    _session: Arc<LeagueSession>,
    _anthropic: anthropic::Anthropic,
    _news_fetcher: Arc<news::NewsFetcher>,
    _cfg: config::Config,
) -> Result<()> {
    anyhow::bail!("rebuild with `--features gui` for the desktop GUI")
}

async fn cmd_roster(session: &LeagueSession) -> Result<()> {
    let week = session.current_week().await?;
    let r = session.my_roster(week).await?;
    println!(
        "{} ({}-{}-{}, PF {:.1}, PA {:.1}) — week {week}",
        r.team_name, r.wins, r.losses, r.ties, r.points_for, r.points_against
    );
    for p in &r.players {
        println!(
            "  {:<5} {:<24} {:<3} {:<4} {:<4} proj {:>5.1}",
            p.roster_slot.to_string(),
            p.name,
            p.position.to_string(),
            p.team,
            p.status.to_string(),
            p.projected_points
        );
    }
    Ok(())
}

async fn cmd_info(session: &LeagueSession) -> Result<()> {
    let s = session.league_settings().await?;
    let w = session.current_week().await?;
    println!("League:       {}", session.league_id);
    println!("Season:       {}", session.season);
    println!("Scoring:      {}", s.scoring);
    println!("Teams:        {}", s.team_count);
    println!("Current week: {w}");
    println!("Roster slots:");
    for (p, c) in &s.roster_slots {
        println!("  {p}: {c}");
    }
    Ok(())
}

async fn cmd_lineup(
    session: &LeagueSession,
    anthropic: &anthropic::Anthropic,
    news_fetcher: &news::NewsFetcher,
    cfg: &config::Config,
    week_override: Option<u8>,
) -> Result<()> {
    let week = match week_override {
        Some(w) => w,
        None => session.current_week().await?,
    };
    let settings = session.league_settings().await.unwrap_or_default();
    let roster = session.my_roster(week).await?;
    let matchups = session.matchups(week).await.unwrap_or_default();
    let mut feed = news_fetcher.fetch_all(50).await;
    let names: Vec<String> = roster.players.iter().map(|p| p.name.clone()).collect();
    let filtered = news::relevant_to(&feed, &names);
    if !filtered.is_empty() {
        feed = filtered;
    }
    let l = lineup::ai_optimize(
        anthropic,
        &roster,
        &settings,
        &matchups,
        &feed,
        cfg.settings.strategy,
        week,
    )
    .await?;
    println!(
        "Strategy: {}  |  Week: {}  |  Projected: {:.1}",
        cfg.settings.strategy.label(),
        week,
        l.projected_total
    );
    for s in &l.starters {
        let name = s
            .player
            .as_ref()
            .map(|p| format!("{} ({} {}) {} proj {:.1}", p.name, p.position, p.team, p.status, p.projected_points))
            .unwrap_or_else(|| "(empty)".into());
        println!("  {:<6} {}", s.slot.to_string(), name);
    }
    println!("\nReasoning:\n{}", l.reasoning);
    Ok(())
}

async fn cmd_waiver(
    session: &LeagueSession,
    anthropic: &anthropic::Anthropic,
    news_fetcher: &news::NewsFetcher,
    cfg: &config::Config,
    pool: usize,
) -> Result<()> {
    let feed = news_fetcher.fetch_all(50).await;
    let report = waiver::analyze(session, anthropic, cfg.settings.strategy, &feed, pool).await?;
    println!(
        "Waiver candidates ({}) — strategy {}",
        report.candidates.len(),
        cfg.settings.strategy.label()
    );
    for c in &report.candidates {
        let drop = c
            .drop_candidate
            .as_ref()
            .map(|d| format!("drop {} (Δ {:+.0})", d.player.name, d.net_ros_delta))
            .unwrap_or_else(|| "no drop".into());
        println!(
            "  {}. {:<22} {:<3} {:<4} proj {:>4.1}/wk ROS {:>4.0} risk {:.2}{} | {}",
            c.priority,
            c.player.name,
            c.player.position.to_string(),
            c.player.team,
            c.metrics.adjusted_next_week,
            c.metrics.ros_value,
            c.metrics.risk_score,
            c.trending_adds
                .map(|n| format!("  {n} adds/24h"))
                .unwrap_or_default(),
            drop,
        );
    }
    if !report.raw.is_empty() {
        println!("\nClaude analysis:\n{}", report.raw);
    }
    Ok(())
}

async fn cmd_trade(
    session: &LeagueSession,
    anthropic: &anthropic::Anthropic,
    news_fetcher: &news::NewsFetcher,
    cfg: &config::Config,
    partner: String,
    send: String,
    receive: String,
) -> Result<()> {
    let week = session.current_week().await?;
    let my_roster = session.my_roster(week).await?;
    let all = session.all_rosters(week).await?;
    let feed = news_fetcher.fetch_all(50).await;
    // Filter empty tokens ("CMC," would otherwise substring-match anything).
    let send_v: Vec<String> = send
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let recv_v: Vec<String> = receive
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let a = trade::analyze(
        anthropic,
        &my_roster,
        &partner,
        &send_v,
        &recv_v,
        &all,
        cfg.settings.strategy,
        &feed,
    )
    .await?;
    println!(
        "Verdict: {}   net ROS {:+.1}   fairness {:.0}%",
        a.verdict,
        a.net_ros_delta,
        a.fairness * 100.0
    );
    println!("\nYou send:");
    for m in &a.send {
        println!("  {}", m.one_line());
    }
    println!("You receive:");
    for m in &a.receive {
        println!("  {}", m.one_line());
    }
    if !a.ai_summary.is_empty() {
        println!("\nClaude analysis:\n{}", a.ai_summary);
    }
    Ok(())
}

async fn cmd_trending(session: &LeagueSession) -> Result<()> {
    let adds = session.trending_players(types::TrendDirection::Add, 25).await?;
    let drops = session.trending_players(types::TrendDirection::Drop, 25).await?;
    println!("Trending ADDS (24h, league-wide):");
    for t in &adds {
        println!(
            "  {:>7}  {:<24} {:<3} {:<4} proj {:>4.1}",
            t.count,
            t.player.name,
            t.player.position.to_string(),
            t.player.team,
            t.player.projected_points
        );
    }
    println!("\nTrending DROPS (24h, league-wide):");
    for t in &drops {
        println!(
            "  {:>7}  {:<24} {:<3} {:<4}",
            t.count,
            t.player.name,
            t.player.position.to_string(),
            t.player.team
        );
    }
    Ok(())
}

async fn cmd_transactions(session: &LeagueSession, weeks: u8) -> Result<()> {
    let week = session.current_week().await?;
    let txs = session.recent_transactions(week, weeks).await?;
    if txs.is_empty() {
        println!("No transactions in the last {weeks} week(s).");
        return Ok(());
    }
    for t in &txs {
        let adds: Vec<String> = t.adds.iter().map(|(p, tm)| format!("{tm} +{p}")).collect();
        let drops: Vec<String> = t.drops.iter().map(|(p, tm)| format!("{tm} -{p}")).collect();
        println!(
            "wk{:<2} {:<12} [{}] {} {}{}{}",
            t.week,
            t.kind,
            t.status,
            adds.join(", "),
            drops.join(", "),
            t.waiver_bid.map(|b| format!(" (${b} FAAB)")).unwrap_or_default(),
            if t.traded_picks.is_empty() {
                String::new()
            } else {
                format!(" | picks: {}", t.traded_picks.join("; "))
            }
        );
    }
    Ok(())
}

async fn cmd_bracket(session: &LeagueSession) -> Result<()> {
    let (winners, losers) = session.playoff_bracket().await?;
    if winners.is_empty() && losers.is_empty() {
        println!("No playoff bracket yet (available once playoffs are set).");
        return Ok(());
    }
    let print_bracket = |title: &str, matches: &[types::BracketMatch]| {
        if matches.is_empty() {
            return;
        }
        println!("{title}:");
        for m in matches {
            println!(
                "  R{} M{}: {} vs {}{}{}",
                m.round,
                m.match_id,
                m.team1.as_deref().unwrap_or("TBD"),
                m.team2.as_deref().unwrap_or("TBD"),
                m.winner
                    .as_deref()
                    .map(|w| format!("  → {w}"))
                    .unwrap_or_default(),
                m.placing
                    .map(|p| format!("  (decides place {p})"))
                    .unwrap_or_default(),
            );
        }
    };
    print_bracket("Winners bracket", &winners);
    print_bracket("Losers bracket", &losers);
    Ok(())
}

async fn cmd_traded_picks(session: &LeagueSession) -> Result<()> {
    let picks = session.traded_picks().await?;
    if picks.is_empty() {
        println!("No traded picks.");
        return Ok(());
    }
    for p in &picks {
        println!(
            "  {} round {}  {} → {}",
            p.season, p.round, p.original_owner, p.current_owner
        );
    }
    Ok(())
}

async fn cmd_draft_suggest(
    session: &LeagueSession,
    anthropic: &anthropic::Anthropic,
    news_fetcher: &news::NewsFetcher,
    cfg: &config::Config,
) -> Result<()> {
    let week = session.current_week().await?;
    let roster = session.my_roster(week).await?;
    let feed = news_fetcher.fetch_all(20).await;
    let dm = draft::DraftManager {
        session,
        anthropic,
        strategy: cfg.settings.strategy,
        my_team_name: roster.team_name.clone(),
    };
    let state = dm.snapshot().await?;
    let s = dm.ask_claude(&state, &feed).await?;
    println!(
        "Pick #{} (round {} of {}){}",
        state.current_pick,
        ((state.current_pick.saturating_sub(1) / state.team_count.max(1)) + 1),
        state.total_rounds,
        state
            .on_the_clock_team
            .as_deref()
            .map(|t| format!(" — {t} on the clock"))
            .unwrap_or_default()
    );
    for p in &s.picks {
        println!(
            "  {}. {} ({}) — {}",
            p.rank,
            p.name,
            p.position.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
            p.rationale
        );
    }
    if s.picks.is_empty() {
        println!("\n--- raw ---\n{}", s.raw);
    }
    Ok(())
}

async fn cmd_draft_watch(
    session: &LeagueSession,
    anthropic: &anthropic::Anthropic,
    news_fetcher: &news::NewsFetcher,
    cfg: &config::Config,
    interval: u64,
) -> Result<()> {
    let week = session.current_week().await?;
    let roster = session.my_roster(week).await?;
    let dm = draft::DraftManager {
        session,
        anthropic,
        strategy: cfg.settings.strategy,
        my_team_name: roster.team_name.clone(),
    };
    println!(
        "Watching draft for '{}' — {} — every {interval}s",
        roster.team_name,
        cfg.settings.strategy.label()
    );
    // interval 0 would busy-loop against the Sleeper API.
    let interval = interval.max(1);
    let mut last_pick = 0u32;
    let mut suggested_for = 0u32;
    loop {
        match dm.snapshot().await {
            Ok(state) => {
                if state.completed {
                    println!("Draft complete.");
                    break;
                }
                // Print every pick made since the last poll, not just the latest.
                let mut new_picks: Vec<&types::DraftPick> = state
                    .picks
                    .iter()
                    .filter(|p| p.pick_number > last_pick)
                    .collect();
                new_picks.sort_by_key(|p| p.pick_number);
                for p in new_picks {
                    last_pick = p.pick_number;
                    println!(
                        "  R{}.{} {} → {}",
                        p.round,
                        p.pick_number,
                        p.team_name,
                        p.player_name.as_deref().unwrap_or("?")
                    );
                }
                if dm.is_my_turn(&state) && state.current_pick != suggested_for {
                    suggested_for = state.current_pick;
                    let feed = news_fetcher.fetch_all(20).await;
                    match dm.ask_claude(&state, &feed).await {
                        Ok(s) => {
                            println!("\n>>> YOU ARE ON THE CLOCK — pick #{} <<<", state.current_pick);
                            for p in &s.picks {
                                println!(
                                    "  {}. {} ({}) — {}",
                                    p.rank,
                                    p.name,
                                    p.position.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
                                    p.rationale
                                );
                            }
                            println!();
                        }
                        Err(e) => eprintln!("draft AI error: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("draft snapshot error: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
    Ok(())
}

fn write_starter_config() -> Result<()> {
    let path = std::path::Path::new("config.yaml");
    if path.exists() {
        anyhow::bail!("config.yaml already exists; refusing to overwrite");
    }
    std::fs::write(path, include_str!("../examples/config.yaml"))?;
    println!("wrote config.yaml — set sleeper.username, then run `sa leagues`.");
    Ok(())
}
