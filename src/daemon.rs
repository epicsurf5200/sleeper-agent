//! Headless monitoring loop, intended to run on a always-on box (Proxmox LXC,
//! VM, container) and nudge you when something needs a decision.
//!
//! Each cycle runs the enabled triggers, compares what it finds against the
//! last thing it told you, and only sends when the content has actually
//! changed. Without that dedupe a 3-hour interval would re-send the same
//! "start Bijan over Javonte" alert eight times a day.

use crate::api::LeagueSession;
use crate::config::Config;
use crate::news::{self, NewsItem};
use crate::notify::{Alert, AlertKind, Notifier};
use crate::types::*;
use crate::{anthropic::Anthropic, lineup, trade, waiver};
use anyhow::{Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

/// Remembers the last alert sent per kind so repeats stay quiet.
#[derive(Default)]
struct SeenState {
    fingerprints: HashMap<String, String>,
    path: PathBuf,
}

impl SeenState {
    fn load() -> Self {
        let path = state_path();
        let fingerprints = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { fingerprints, path }
    }

    /// True when this is new information worth sending.
    fn is_new(&self, kind: AlertKind, fingerprint: &str) -> bool {
        self.fingerprints.get(kind.label()).map(String::as_str) != Some(fingerprint)
    }

    fn record(&mut self, kind: AlertKind, fingerprint: &str) {
        self.fingerprints
            .insert(kind.label().to_string(), fingerprint.to_string());
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.fingerprints) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

fn state_path() -> PathBuf {
    if let Ok(dir) = std::env::var("SA_CACHE_DIR") {
        return PathBuf::from(dir).join("daemon-state.json");
    }
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sleeper-agent")
        .join("daemon-state.json")
}

fn fingerprint(parts: &[&str]) -> String {
    let mut h = DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    format!("{:x}", h.finish())
}

/// Players we currently have slotted as starters in Sleeper.
fn current_starters(roster: &Roster) -> Vec<&Player> {
    roster
        .players
        .iter()
        .filter(|p| p.roster_slot != Position::BENCH && p.roster_slot != Position::IR)
        .collect()
}

pub struct DaemonArgs {
    /// Run a single cycle and exit — for testing the wiring and for cron.
    pub once: bool,
    /// Build and print alerts without sending them anywhere.
    pub dry_run: bool,
}

pub async fn run(
    cfg: &Config,
    session: &LeagueSession,
    anthropic: &Anthropic,
    args: DaemonArgs,
) -> Result<()> {
    let notifier = Notifier::new(&cfg.notify)?;
    if notifier.is_none() && !args.dry_run {
        anyhow::bail!(
            "no webhook configured — set notify.webhook_url in {} or the SA_WEBHOOK_URL env var \
             (or pass --dry-run to preview alerts)",
            cfg.path.display()
        );
    }
    let mut state = SeenState::load();
    let interval = Duration::from_secs(cfg.daemon.interval_minutes.max(1) * 60);
    let t = &cfg.daemon.triggers;

    tracing::info!(
        interval_minutes = cfg.daemon.interval_minutes,
        active_hours = format!("{}-{}", cfg.daemon.active_hour_start, cfg.daemon.active_hour_end),
        lineup = t.better_lineup,
        injury = t.injured_starter,
        waiver = t.waiver,
        trade = t.trade,
        "daemon started"
    );

    loop {
        // An explicit --once run is the operator asking for a cycle right now
        // (or cron, which does its own scheduling) — quiet hours don't apply.
        if args.once || within_active_hours(cfg) {
            // One bad cycle (Sleeper 5xx, rate limit) must not kill the daemon.
            match cycle(cfg, session, anthropic, &mut state, notifier.as_ref(), &args).await {
                Ok(n) => tracing::info!(alerts = n, "cycle complete"),
                Err(e) => tracing::error!(error = %e, "cycle failed"),
            }
        } else {
            tracing::debug!("outside active hours, skipping cycle");
        }
        if args.once {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}

fn within_active_hours(cfg: &Config) -> bool {
    let (start, end) = (cfg.daemon.active_hour_start, cfg.daemon.active_hour_end);
    if start == end {
        return true; // degenerate config means "always on"
    }
    let hour = chrono::Local::now().format("%H").to_string().parse::<u32>().unwrap_or(12);
    if start < end {
        hour >= start && hour < end
    } else {
        // Window wraps past midnight, e.g. 22 → 6.
        hour >= start || hour < end
    }
}

/// Runs every enabled trigger once. Returns how many alerts were sent.
async fn cycle(
    cfg: &Config,
    session: &LeagueSession,
    anthropic: &Anthropic,
    state: &mut SeenState,
    notifier: Option<&Notifier>,
    args: &DaemonArgs,
) -> Result<usize> {
    let week = session.current_week().await.context("fetching current week")?;
    let roster = session.my_roster(week).await.context("fetching my roster")?;
    let settings = session.league_settings().await.context("fetching league settings")?;

    // Pre-draft the roster is legitimately empty. Every trigger would either
    // fire uselessly or ask Claude to reason about nothing, so stop here.
    if roster.players.is_empty() {
        tracing::info!("roster is empty (league has not drafted yet) — nothing to analyse");
        return Ok(0);
    }

    let names: Vec<String> = roster.players.iter().map(|p| p.name.clone()).collect();
    let news_items = match news::NewsFetcher::new(cfg.settings.news_sources.clone()) {
        Ok(f) => news::relevant_to(&f.fetch_all(40).await, &names),
        Err(e) => {
            tracing::warn!(error = %e, "news fetch unavailable");
            Vec::new()
        }
    };

    let mut alerts: Vec<Alert> = Vec::new();
    let t = &cfg.daemon.triggers;

    if t.injured_starter {
        if let Some(a) = injury_alert(&roster) {
            alerts.push(a);
        }
    }

    // The three AI triggers are independent, so run them concurrently rather
    // than end to end — a cycle costs the slowest call instead of their sum.
    // Each is 10-60s, so this is the difference between a ~2 minute cycle and
    // a ~1 minute one. Note this puts up to three completions in flight at
    // once; on the `claude-cli` backend that means three CLI processes, which
    // matters on a memory-tight LXC (see deploy/README.md).
    let lineup_fut = async {
        if !t.better_lineup {
            return None;
        }
        lineup_alert(anthropic, &roster, &settings, &news_items, cfg, week)
            .await
            .map_err(|e| tracing::warn!(error = %e, "lineup trigger failed"))
            .ok()
            .flatten()
    };
    let waiver_fut = async {
        if !t.waiver {
            return None;
        }
        waiver_alert(session, anthropic, cfg, &news_items)
            .await
            .map_err(|e| tracing::warn!(error = %e, "waiver trigger failed"))
            .ok()
            .flatten()
    };
    let trade_fut = async {
        if !t.trade {
            return None;
        }
        trade_alert(session, anthropic, cfg, &roster, &news_items, week)
            .await
            .map_err(|e| tracing::warn!(error = %e, "trade trigger failed"))
            .ok()
            .flatten()
    };

    // Collected in a fixed order so alert ordering stays deterministic
    // regardless of which completion happens to return first.
    let (lineup_a, waiver_a, trade_a) = tokio::join!(lineup_fut, waiver_fut, trade_fut);
    alerts.extend([lineup_a, waiver_a, trade_a].into_iter().flatten());

    let mut sent = 0;
    for alert in alerts {
        if !state.is_new(alert.kind, &alert.fingerprint) {
            tracing::debug!(kind = alert.kind.label(), "suppressed duplicate");
            continue;
        }
        if args.dry_run {
            // Print only — and deliberately don't record the fingerprint, so a
            // dry run stays side-effect-free and doesn't suppress the real
            // alert the next cycle would have sent.
            println!("\n=== [{}] {} ===\n{}", alert.kind.label(), alert.title, alert.body);
        } else {
            if let Some(n) = notifier {
                n.send(&alert).await.context("sending alert")?;
            }
            state.record(alert.kind, &alert.fingerprint);
        }
        sent += 1;
    }
    Ok(sent)
}

fn injury_alert(roster: &Roster) -> Option<Alert> {
    let hurt: Vec<&Player> = current_starters(roster)
        .into_iter()
        .filter(|p| {
            matches!(
                p.status,
                PlayerStatus::Out
                    | PlayerStatus::Doubtful
                    | PlayerStatus::IR
                    | PlayerStatus::Suspended
            )
        })
        .collect();
    if hurt.is_empty() {
        return None;
    }
    let body = hurt
        .iter()
        .map(|p| format!("• **{}** ({} {}) — {}", p.name, p.position, p.team, p.status))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ids: Vec<String> = hurt.iter().map(|p| format!("{}:{}", p.id, p.status)).collect();
    ids.sort();
    let fp = fingerprint(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    Some(Alert::new(
        AlertKind::Injury,
        format!("{} starter(s) not expected to play", hurt.len()),
        format!("{body}\n\nThese are in your starting lineup right now."),
        fp,
    ))
}

async fn lineup_alert(
    anthropic: &Anthropic,
    roster: &Roster,
    settings: &LeagueSettings,
    news: &[NewsItem],
    cfg: &Config,
    week: u8,
) -> Result<Option<Alert>> {
    let rec = lineup::ai_optimize(
        anthropic,
        roster,
        settings,
        &[],
        news,
        cfg.settings.strategy,
        week,
    )
    .await?;

    let recommended: HashSet<&str> = rec
        .starters
        .iter()
        .filter_map(|s| s.player.as_ref().map(|p| p.id.as_str()))
        .collect();
    let current: HashSet<&str> = current_starters(roster).iter().map(|p| p.id.as_str()).collect();

    // No lineup set at all is itself worth a nudge.
    if current.is_empty() {
        let body = rec
            .starters
            .iter()
            .map(|s| {
                format!(
                    "{}: {}",
                    s.slot,
                    s.player.as_ref().map(|p| p.name.as_str()).unwrap_or("(empty)")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut ids: Vec<&str> = recommended.into_iter().collect();
        ids.sort_unstable();
        return Ok(Some(Alert::new(
            AlertKind::Lineup,
            format!("Week {week}: no lineup set"),
            format!("Suggested lineup:\n{body}\n\n{}", rec.reasoning),
            fingerprint(&ids),
        )));
    }

    if recommended == current {
        return Ok(None);
    }

    let name_of = |id: &str| {
        roster
            .players
            .iter()
            .find(|p| p.id == id)
            .map(|p| format!("{} ({} {})", p.name, p.position, p.team))
            .unwrap_or_else(|| id.to_string())
    };
    let bench_them: Vec<String> = current.difference(&recommended).map(|id| name_of(id)).collect();
    let start_them: Vec<String> = recommended.difference(&current).map(|id| name_of(id)).collect();

    let body = format!(
        "**Start:**\n{}\n\n**Bench:**\n{}\n\n{}",
        start_them.iter().map(|s| format!("• {s}")).collect::<Vec<_>>().join("\n"),
        bench_them.iter().map(|s| format!("• {s}")).collect::<Vec<_>>().join("\n"),
        rec.reasoning
    );
    let mut ids: Vec<&str> = recommended.into_iter().collect();
    ids.sort_unstable();
    Ok(Some(Alert::new(
        AlertKind::Lineup,
        format!("Week {week}: better lineup available ({} change(s))", start_them.len()),
        body,
        fingerprint(&ids),
    )))
}

async fn waiver_alert(
    session: &LeagueSession,
    anthropic: &Anthropic,
    cfg: &Config,
    news: &[NewsItem],
) -> Result<Option<Alert>> {
    let report = waiver::analyze(session, anthropic, cfg.settings.strategy, news, 60).await?;
    let top: Vec<_> = report.candidates.iter().take(3).collect();
    if top.is_empty() {
        return Ok(None);
    }
    let body = top
        .iter()
        .map(|c| {
            let drop = c
                .drop_candidate
                .as_ref()
                .map(|d| format!(" — drop {}", d.player.name))
                .unwrap_or_default();
            format!(
                "**{}. {}** ({} {}){}\n{}",
                c.priority, c.player.name, c.player.position, c.player.team, drop, c.reasoning
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let ids: Vec<&str> = top.iter().map(|c| c.player.id.as_str()).collect();
    Ok(Some(Alert::new(
        AlertKind::Waiver,
        format!("{} waiver target(s) worth a claim", top.len()),
        body,
        fingerprint(&ids),
    )))
}

async fn trade_alert(
    session: &LeagueSession,
    anthropic: &Anthropic,
    cfg: &Config,
    roster: &Roster,
    news: &[NewsItem],
    week: u8,
) -> Result<Option<Alert>> {
    let all = session.all_rosters(week).await?;
    let text = trade::suggest(anthropic, roster, &all, cfg.settings.strategy, news, 2).await?;
    if text.trim().is_empty() || text.to_uppercase().contains("NO ACTION") {
        return Ok(None);
    }
    Ok(Some(Alert::new(
        AlertKind::Trade,
        "Trade ideas worth exploring",
        text.clone(),
        fingerprint(&[text.trim()]),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;

    fn cfg_with_hours(start: u32, end: u32) -> Config {
        let mut c = Config {
            anthropic: Default::default(),
            sleeper: Default::default(),
            settings: Default::default(),
            notify: Default::default(),
            daemon: DaemonConfig::default(),
            base_dir: Default::default(),
            path: Default::default(),
            api_key_from_env: false,
            webhook_from_env: false,
        };
        c.daemon.active_hour_start = start;
        c.daemon.active_hour_end = end;
        c
    }

    fn player(name: &str, slot: Position, status: PlayerStatus) -> Player {
        Player {
            id: name.to_lowercase().replace(' ', "_"),
            name: name.into(),
            position: Position::RB,
            roster_slot: slot,
            team: "SF".into(),
            projected_points: 10.0,
            avg_points: 10.0,
            status,
            opponent: None,
            bye_week: None,
            news: vec![],
        }
    }

    fn roster(players: Vec<Player>) -> Roster {
        Roster {
            team_id: "1".into(),
            team_name: "Testers".into(),
            owner: None,
            players,
            wins: 0,
            losses: 0,
            ties: 0,
            points_for: 0.0,
            points_against: 0.0,
        }
    }

    #[test]
    fn injury_alert_ignores_bench_players() {
        let r = roster(vec![
            player("Benched Guy", Position::BENCH, PlayerStatus::Out),
            player("Healthy Starter", Position::RB, PlayerStatus::Healthy),
        ]);
        assert!(injury_alert(&r).is_none(), "only starters should trigger");
    }

    #[test]
    fn injury_alert_fires_on_out_starter() {
        let r = roster(vec![player("Hurt Starter", Position::RB, PlayerStatus::Out)]);
        let a = injury_alert(&r).expect("should alert");
        assert_eq!(a.kind, AlertKind::Injury);
        assert!(a.body.contains("Hurt Starter"));
    }

    #[test]
    fn injury_fingerprint_changes_when_status_changes() {
        let a = injury_alert(&roster(vec![player("X", Position::RB, PlayerStatus::Doubtful)]));
        let b = injury_alert(&roster(vec![player("X", Position::RB, PlayerStatus::Out)]));
        assert_ne!(a.unwrap().fingerprint, b.unwrap().fingerprint);
    }

    #[test]
    fn dedupe_suppresses_only_identical_content() {
        let mut s = SeenState::default();
        assert!(s.is_new(AlertKind::Waiver, "abc"));
        s.fingerprints.insert("Waiver".into(), "abc".into());
        assert!(!s.is_new(AlertKind::Waiver, "abc"));
        assert!(s.is_new(AlertKind::Waiver, "xyz"));
        // Kinds are independent namespaces.
        assert!(s.is_new(AlertKind::Injury, "abc"));
    }

    #[test]
    fn active_hours_handle_midnight_wrap() {
        // Normal window.
        let c = cfg_with_hours(8, 23);
        assert_eq!(c.daemon.active_hour_start, 8);
        // Degenerate window means always-on.
        assert!(within_active_hours(&cfg_with_hours(0, 0)));
    }
}
