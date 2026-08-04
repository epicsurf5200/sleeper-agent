//! Backtest the AI manager against a real historical season.
//!
//! Drafts a realistic 12-team league from that season's week-1 projections,
//! then replays the season week by week: Claude sets our lineup from the
//! real weekly projections, and every decision is scored with the week's
//! REAL actual fantasy points. Baselines put the AI's value in context:
//!
//!   naive    — set a week-1 lineup and never touch it again
//!   greedy   — re-optimize weekly on projections (no AI)
//!   ai       — Claude's lineup (real reasoning, real API/subscription call)
//!   optimal  — perfect hindsight (best possible with actual points)

use crate::anthropic::Anthropic;
use crate::api::SleeperClient;
use crate::strategy::Strategy;
use crate::types::*;
use crate::{lineup, news::NewsItem};
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};

const TEAMS: usize = 12;
const ROUNDS: usize = 15;

pub struct BacktestArgs {
    pub season: String,
    pub weeks: u8,
    pub slot: usize,
    pub strategy: Strategy,
}

fn league_settings() -> LeagueSettings {
    LeagueSettings {
        scoring: "ppr".into(),
        roster_slots: vec![
            (Position::QB, 1),
            (Position::RB, 2),
            (Position::WR, 2),
            (Position::TE, 1),
            (Position::FLEX, 1),
            (Position::DST, 1),
            (Position::K, 1),
            (Position::BENCH, 6),
        ],
        team_count: TEAMS as u32,
    }
}

/// Points for a projection/stat entry under ppr scoring.
fn pts(map: &HashMap<String, HashMap<String, f64>>, id: &str) -> f32 {
    map.get(id)
        .and_then(|m| m.get("pts_ppr"))
        .copied()
        .unwrap_or(0.0) as f32
}

struct PoolPlayer {
    id: String,
    name: String,
    pos: Position,
    team: String,
    w1_proj: f32,
}

/// Needs-aware snake draft over the ranked pool. Returns rosters per team slot.
fn snake_draft(pool: &[PoolPlayer]) -> Vec<Vec<usize>> {
    let mut rosters: Vec<Vec<usize>> = vec![Vec::new(); TEAMS];
    let mut taken = vec![false; pool.len()];
    for overall in 0..TEAMS * ROUNDS {
        let round = overall / TEAMS;
        let idx = overall % TEAMS;
        let team = if round % 2 == 1 { TEAMS - 1 - idx } else { idx };
        let mut have: HashMap<Position, u32> = HashMap::new();
        for &pi in &rosters[team] {
            *have.entry(pool[pi].pos).or_insert(0) += 1;
        }
        let need = |p: Position, have: &HashMap<Position, u32>| -> bool {
            let h = have.get(&p).copied().unwrap_or(0);
            match p {
                Position::QB => h < 2,
                Position::TE => h < 2,
                Position::K | Position::DST => h < 1 && round >= ROUNDS - 3,
                Position::RB | Position::WR => h < 7,
                _ => false,
            }
        };
        // Force K/DST in the final rounds if still missing.
        let forced = if round >= ROUNDS - 2 {
            [Position::K, Position::DST]
                .into_iter()
                .find(|p| have.get(p).copied().unwrap_or(0) == 0)
        } else {
            None
        };
        let pick = pool
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                !taken[*i]
                    && match forced {
                        Some(fp) => p.pos == fp,
                        None => need(p.pos, &have),
                    }
            })
            .max_by(|a, b| a.1.w1_proj.partial_cmp(&b.1.w1_proj).unwrap());
        if let Some((i, _)) = pick {
            taken[i] = true;
            rosters[team].push(i);
        }
    }
    rosters
}

fn build_roster(
    pool: &[PoolPlayer],
    picks: &[usize],
    proj: &HashMap<String, HashMap<String, f64>>,
) -> Roster {
    Roster {
        team_id: "backtest".into(),
        team_name: "Backtest AI".into(),
        owner: Some("backtest".into()),
        players: picks
            .iter()
            .map(|&i| {
                let p = &pool[i];
                Player {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    position: p.pos,
                    roster_slot: Position::BENCH,
                    team: p.team.clone(),
                    projected_points: pts(proj, &p.id),
                    avg_points: 0.0,
                    status: PlayerStatus::Healthy,
                    opponent: None,
                    bye_week: None,
                    news: vec![],
                }
            })
            .collect(),
        wins: 0,
        losses: 0,
        ties: 0,
        points_for: 0.0,
        points_against: 0.0,
    }
}

/// Actual points scored by a set of starters.
fn score(starters: &[LineupSlot], stats: &HashMap<String, HashMap<String, f64>>) -> f32 {
    starters
        .iter()
        .filter_map(|s| s.player.as_ref())
        .map(|p| pts(stats, &p.id))
        .sum()
}

pub async fn run(client: &SleeperClient, anthropic: &Anthropic, args: BacktestArgs) -> Result<()> {
    let settings = league_settings();
    println!(
        "Backtest: {} season, weeks 1-{}, draft slot {}/{}, strategy {}",
        args.season,
        args.weeks,
        args.slot,
        TEAMS,
        args.strategy.label()
    );
    println!("Scoring: real Sleeper actuals (pts_ppr). AI backend: live Claude.\n");

    // -- draft from week-1 projections (no hindsight) ------------------------
    let players = client.all_players().await?;
    let w1 = client.projections(&args.season, 1).await?;
    let mut pool: Vec<PoolPlayer> = players
        .values()
        .filter_map(|sp| {
            let pos = Position::from_str(sp.position.as_deref()?);
            if !matches!(
                pos,
                Position::QB | Position::RB | Position::WR | Position::TE | Position::K | Position::DST
            ) {
                return None;
            }
            let team = sp.team.clone()?;
            if team.is_empty() {
                return None;
            }
            let name = sp.full_name.clone().or_else(|| {
                Some(format!("{} {}", sp.first_name.clone()?, sp.last_name.clone()?))
            })?;
            let w1_proj = pts(&w1, &sp.player_id);
            if w1_proj <= 0.0 {
                return None;
            }
            Some(PoolPlayer { id: sp.player_id.clone(), name, pos, team, w1_proj })
        })
        .collect();
    pool.sort_by(|a, b| b.w1_proj.partial_cmp(&a.w1_proj).unwrap());
    if pool.len() < TEAMS * ROUNDS {
        return Err(anyhow!("player pool too small: {}", pool.len()));
    }

    let rosters = snake_draft(&pool);
    let my_picks = rosters
        .get(args.slot.saturating_sub(1))
        .ok_or_else(|| anyhow!("slot must be 1-{TEAMS}"))?;
    println!("Drafted (slot {}):", args.slot);
    for &i in my_picks {
        let p = &pool[i];
        println!("  {:<4} {:<26} {} (w1 proj {:.1})", p.pos.to_string(), p.name, p.team, p.w1_proj);
    }
    println!();

    // Naive manager: week-1 greedy lineup, never touched again.
    let w1_roster = build_roster(&pool, my_picks, &w1);
    let naive_lineup = lineup::local_optimize(&w1_roster, &settings, args.strategy, 1);
    let naive_ids: Vec<Option<String>> = naive_lineup
        .starters
        .iter()
        .map(|s| s.player.as_ref().map(|p| p.id.clone()))
        .collect();

    // -- replay the season ---------------------------------------------------
    let (mut tot_ai, mut tot_greedy, mut tot_naive, mut tot_opt) = (0f32, 0f32, 0f32, 0f32);
    let mut ai_wins_vs_greedy = 0u32;
    let empty_news: Vec<NewsItem> = Vec::new();
    println!(
        "{:<5} {:>8} {:>8} {:>8} {:>8}   {}",
        "week", "AI", "greedy", "naive", "optimal", "AI reasoning (snippet)"
    );

    for week in 1..=args.weeks {
        let proj = client.projections(&args.season, week).await?;
        let stats = client.stats(&args.season, week).await?;
        let roster = build_roster(&pool, my_picks, &proj);

        // AI lineup (real Claude call on real projections).
        let ai = lineup::ai_optimize(
            anthropic,
            &roster,
            &settings,
            &[],
            &empty_news,
            args.strategy,
            week,
        )
        .await?;
        let ai_pts = score(&ai.starters, &stats);

        // Greedy: weekly local optimizer on projections.
        let greedy = lineup::local_optimize(&roster, &settings, args.strategy, week);
        let greedy_pts = score(&greedy.starters, &stats);

        // Naive: frozen week-1 lineup.
        let naive_pts: f32 = naive_ids
            .iter()
            .filter_map(|id| id.as_ref())
            .map(|id| pts(&stats, id))
            .sum();

        // Optimal hindsight: optimize on actuals.
        let stats_owned: HashMap<String, HashMap<String, f64>> = stats.clone();
        let hindsight_roster = build_roster(&pool, my_picks, &stats_owned);
        let optimal = lineup::local_optimize(&hindsight_roster, &settings, args.strategy, week);
        let opt_pts = score(&optimal.starters, &stats);

        tot_ai += ai_pts;
        tot_greedy += greedy_pts;
        tot_naive += naive_pts;
        tot_opt += opt_pts;
        if ai_pts >= greedy_pts {
            ai_wins_vs_greedy += 1;
        }

        let parse_failed = ai.reasoning.contains("parse failed") || ai.reasoning.contains("AI unavailable");
        let snippet: String = ai.reasoning.chars().take(72).collect();
        println!(
            "{:<5} {:>8.1} {:>8.1} {:>8.1} {:>8.1}   {}{}",
            week,
            ai_pts,
            greedy_pts,
            naive_pts,
            opt_pts,
            snippet,
            if parse_failed { "  [FALLBACK]" } else { "" }
        );
    }

    // -- verdict -------------------------------------------------------------
    let weeks = args.weeks as f32;
    println!("\n===== SEASON TOTALS ({} weeks) =====", args.weeks);
    println!("  AI (Claude):      {:>8.1}  ({:.1}/wk)", tot_ai, tot_ai / weeks);
    println!("  Greedy baseline:  {:>8.1}  ({:.1}/wk)", tot_greedy, tot_greedy / weeks);
    println!("  Naive (set/forget):{:>7.1}  ({:.1}/wk)", tot_naive, tot_naive / weeks);
    println!("  Hindsight optimal:{:>8.1}  ({:.1}/wk)", tot_opt, tot_opt / weeks);
    println!("\n  AI vs greedy:  {:+.1} pts ({:+.1}/wk), better-or-equal {}/{} weeks",
        tot_ai - tot_greedy, (tot_ai - tot_greedy) / weeks, ai_wins_vs_greedy, args.weeks);
    println!("  AI vs naive:   {:+.1} pts ({:+.1}/wk)", tot_ai - tot_naive, (tot_ai - tot_naive) / weeks);
    if tot_opt > 0.0 {
        println!(
            "  Efficiency:    AI captured {:.1}% of the hindsight-optimal points (greedy {:.1}%, naive {:.1}%)",
            100.0 * tot_ai / tot_opt,
            100.0 * tot_greedy / tot_opt,
            100.0 * tot_naive / tot_opt
        );
    }
    Ok(())
}
