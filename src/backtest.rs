//! Backtest the AI manager against a real historical season.
//!
//! Drafts a deterministic 12-team league from that season's week-1 projections,
//! then replays the season week by week: Claude sets our lineup, and every
//! decision is scored with the week's REAL actual fantasy points.
//!
//! The point of the exercise is to isolate *decision quality*, so the AI is
//! given one piece of information the plain projection optimizer does not use:
//! each player's trailing actual PPG through the previous week (strictly no
//! hindsight). Baselines bracket the result:
//!
//!   naive    — set a week-1 lineup and never touch it again
//!   greedy   — re-optimize weekly on projections alone (no form, no AI)
//!   form     — re-optimize weekly on a 60/40 projection/trailing-form blend
//!   ai       — Claude's lineup (real reasoning, real API/subscription call)
//!   optimal  — perfect hindsight (best possible with actual points)
//!
//! `form` is the key addition: it is a dumb heuristic that uses exactly the
//! extra information the AI receives. If the AI cannot beat `greedy`, it is
//! ignoring the signal; if it cannot beat `form`, it is using it worse than a
//! two-line weighted average.

use crate::anthropic::Anthropic;
use crate::api::SleeperClient;
use crate::strategy::Strategy;
use crate::types::*;
use crate::{lineup, news::NewsItem};
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const TEAMS: usize = 12;
const ROUNDS: usize = 15;
/// Weight on the consensus projection in the `form` blend baseline.
const PROJ_WEIGHT: f32 = 0.6;
/// Minimum games played before trailing form is considered meaningful.
const MIN_FORM_GAMES: usize = 2;
/// Attempts per weekly AI call before giving up and recording a fallback.
const AI_ATTEMPTS: usize = 3;

pub struct BacktestArgs {
    pub season: String,
    pub weeks: u8,
    pub slot: usize,
    pub strategy: Strategy,
    /// Skip all AI calls; sweep the form-blend weight instead. Costs no tokens
    /// and answers "is there anything here for a smart manager to win?".
    pub dry: bool,
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

type StatMap = HashMap<String, HashMap<String, f64>>;

/// Points for a projection/stat entry under ppr scoring.
fn pts(map: &StatMap, id: &str) -> f32 {
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
///
/// Ties are broken by player id so the same season is drafted on every run —
/// without this the pool order depends on hash iteration order and each model
/// would play a different season, making totals incomparable.
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
            .max_by(|a, b| {
                a.1.w1_proj
                    .partial_cmp(&b.1.w1_proj)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.1.id.cmp(&a.1.id))
            });
        if let Some((i, _)) = pick {
            taken[i] = true;
            rosters[team].push(i);
        }
    }
    rosters
}

/// Per-week view of one rostered player: what it projects, what it has
/// actually done so far, and whether its team plays at all.
struct WeekFacts {
    proj: f32,
    /// Mean actual PPG over games played strictly before this week.
    form: Option<f32>,
    on_bye: bool,
    /// This week's real result. Hindsight — read ONLY by the `optimal`
    /// ceiling baseline, never surfaced to the AI or the other managers.
    actual: f32,
}

/// Build a roster whose `projected_points` come from `value` and whose
/// `avg_points` carry trailing form only when `expose_form` is set.
fn build_roster(
    pool: &[PoolPlayer],
    picks: &[usize],
    facts: &HashMap<String, WeekFacts>,
    week: u8,
    value: impl Fn(&WeekFacts) -> f32,
    expose_form: bool,
) -> Roster {
    Roster {
        team_id: "backtest".into(),
        team_name: "Backtest AI".into(),
        owner: Some("backtest".into()),
        players: picks
            .iter()
            .map(|&i| {
                let p = &pool[i];
                let f = facts.get(&p.id);
                Player {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    position: p.pos,
                    roster_slot: Position::BENCH,
                    team: p.team.clone(),
                    projected_points: f.map(&value).unwrap_or(0.0),
                    avg_points: if expose_form {
                        f.and_then(|f| f.form).unwrap_or(0.0)
                    } else {
                        0.0
                    },
                    status: PlayerStatus::Healthy,
                    opponent: None,
                    bye_week: f.filter(|f| f.on_bye).map(|_| week),
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
fn score(starters: &[LineupSlot], stats: &StatMap) -> f32 {
    starters
        .iter()
        .filter_map(|s| s.player.as_ref())
        .map(|p| pts(stats, &p.id))
        .sum()
}

fn starter_ids(l: &Lineup) -> HashSet<String> {
    l.starters
        .iter()
        .filter_map(|s| s.player.as_ref().map(|p| p.id.clone()))
        .collect()
}

fn is_fallback(reasoning: &str) -> bool {
    reasoning.contains("parse failed") || reasoning.contains("AI unavailable")
}

pub async fn run(client: &SleeperClient, anthropic: &Anthropic, args: BacktestArgs) -> Result<()> {
    let settings = league_settings();
    println!(
        "Backtest: {} season, weeks 1-{}, draft slot {}/{}, strategy {}, model {}",
        args.season,
        args.weeks,
        args.slot,
        TEAMS,
        args.strategy.label(),
        anthropic.model()
    );
    println!("Scoring: real Sleeper actuals (pts_ppr). AI backend: live Claude.");
    println!(
        "AI sees trailing actual PPG through week-1; greedy does not. Draft is deterministic.\n"
    );

    // -- fetch the whole season up front ------------------------------------
    let players = client.all_players().await?;
    let mut weekly_proj: Vec<Arc<StatMap>> = Vec::new();
    let mut weekly_stats: Vec<StatMap> = Vec::new();
    for w in 1..=args.weeks {
        weekly_proj.push(client.projections(&args.season, w).await?);
        weekly_stats.push(client.stats(&args.season, w).await?);
    }
    let w1 = weekly_proj[0].clone();

    // -- draft from week-1 projections (no hindsight) ------------------------
    let mut pool: Vec<PoolPlayer> = players
        .values()
        .filter_map(|sp| {
            let pos = Position::from_str(sp.position.as_deref()?);
            if !matches!(
                pos,
                Position::QB
                    | Position::RB
                    | Position::WR
                    | Position::TE
                    | Position::K
                    | Position::DST
            ) {
                return None;
            }
            let team = sp.team.clone()?;
            if team.is_empty() {
                return None;
            }
            let name = sp
                .full_name
                .clone()
                .or_else(|| Some(format!("{} {}", sp.first_name.clone()?, sp.last_name.clone()?)))?;
            let w1_proj = pts(&w1, &sp.player_id);
            if w1_proj <= 0.0 {
                return None;
            }
            Some(PoolPlayer { id: sp.player_id.clone(), name, pos, team, w1_proj })
        })
        .collect();
    // Deterministic order: projection desc, then id — no hash-order dependence.
    pool.sort_by(|a, b| {
        b.w1_proj
            .partial_cmp(&a.w1_proj)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
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

    // -- derive bye weeks: a team is on bye if none of its players recorded a
    //    stat line that week. Byes are public knowledge in advance, so telling
    //    the manager about them is not hindsight.
    let mut byes: Vec<HashSet<String>> = Vec::new();
    for stats in weekly_stats.iter() {
        let active: HashSet<&str> = pool
            .iter()
            .filter(|p| stats.contains_key(&p.id))
            .map(|p| p.team.as_str())
            .collect();
        let all_teams: HashSet<&str> = pool.iter().map(|p| p.team.as_str()).collect();
        byes.push(
            all_teams
                .difference(&active)
                .map(|t| t.to_string())
                .collect(),
        );
    }

    // -- per-week facts for our roster ---------------------------------------
    let mut played: HashMap<String, Vec<f32>> = HashMap::new();
    let mut facts_by_week: Vec<HashMap<String, WeekFacts>> = Vec::new();
    for (wi, stats) in weekly_stats.iter().enumerate() {
        let mut facts = HashMap::new();
        for &i in my_picks {
            let p = &pool[i];
            let history = played.get(&p.id).map(|v| v.as_slice()).unwrap_or(&[]);
            let form = if history.len() >= MIN_FORM_GAMES {
                Some(history.iter().sum::<f32>() / history.len() as f32)
            } else {
                None
            };
            facts.insert(
                p.id.clone(),
                WeekFacts {
                    proj: pts(&weekly_proj[wi], &p.id),
                    form,
                    on_bye: byes[wi].contains(&p.team),
                    actual: pts(stats, &p.id),
                },
            );
        }
        // Only after computing this week's facts do we fold in this week's
        // result — trailing form must never include the current week.
        for &i in my_picks {
            let p = &pool[i];
            if !byes[wi].contains(&p.team) && stats.contains_key(&p.id) {
                played.entry(p.id.clone()).or_default().push(pts(stats, &p.id));
            }
        }
        facts_by_week.push(facts);
    }

    // -- dry mode: how much is the form signal actually worth? ---------------
    if args.dry {
        println!("Form-blend weight sweep (no AI calls). w = weight on trailing form:\n");
        println!("{:>6} {:>10} {:>10} {:>8}", "w", "total", "vs greedy", "swaps");
        let mut baseline_total = 0.0f32;
        for step in 0..=10 {
            let w = step as f32 / 10.0;
            let mut total = 0.0f32;
            let mut swaps = 0u32;
            for week in 1..=args.weeks {
                let wi = (week - 1) as usize;
                let facts = &facts_by_week[wi];
                let r = build_roster(&pool, my_picks, facts, week, |f| blend_w(f, w), false);
                let l = lineup::local_optimize(&r, &settings, args.strategy, week);
                total += score(&l.starters, &weekly_stats[wi]);
                let g = build_roster(&pool, my_picks, facts, week, |f| f.proj, false);
                let gl = lineup::local_optimize(&g, &settings, args.strategy, week);
                swaps += starter_ids(&l).difference(&starter_ids(&gl)).count() as u32;
            }
            if step == 0 {
                baseline_total = total;
            }
            println!("{:>6.1} {:>10.1} {:>+10.1} {:>8}", w, total, total - baseline_total, swaps);
        }
        let mut opt = 0.0f32;
        for week in 1..=args.weeks {
            let wi = (week - 1) as usize;
            let r = build_roster(&pool, my_picks, &facts_by_week[wi], week, |f| f.actual, false);
            let l = lineup::local_optimize(&r, &settings, args.strategy, week);
            opt += score(&l.starters, &weekly_stats[wi]);
        }
        println!("\n  hindsight optimal: {:.1}  (headroom over w=0: {:+.1})", opt, opt - baseline_total);
        return Ok(());
    }

    // Naive manager: week-1 greedy lineup, never touched again.
    let w1_roster = build_roster(&pool, my_picks, &facts_by_week[0], 1, |f| f.proj, false);
    let naive_lineup = lineup::local_optimize(&w1_roster, &settings, args.strategy, 1);
    let naive_ids: Vec<String> = starter_ids(&naive_lineup).into_iter().collect();

    // -- replay the season ---------------------------------------------------
    let (mut tot_ai, mut tot_greedy, mut tot_form, mut tot_naive, mut tot_opt) =
        (0f32, 0f32, 0f32, 0f32, 0f32);
    let (mut ai_beat_greedy, mut ai_lost_greedy) = (0u32, 0u32);
    let (mut fallbacks, mut total_swaps, mut weeks_deviated) = (0u32, 0u32, 0u32);
    let empty_news: Vec<NewsItem> = Vec::new();
    println!(
        "{:<5} {:>7} {:>7} {:>7} {:>7} {:>7} {:>4}  AI reasoning (snippet)",
        "week", "AI", "greedy", "form", "naive", "optimal", "swp"
    );

    for week in 1..=args.weeks {
        let wi = (week - 1) as usize;
        let stats = &weekly_stats[wi];
        let facts = &facts_by_week[wi];

        // Rosters differ only in what value each manager optimizes and whether
        // trailing form is visible at all.
        let ai_roster = build_roster(&pool, my_picks, facts, week, |f| f.proj, true);
        let greedy_roster = build_roster(&pool, my_picks, facts, week, |f| f.proj, false);
        let form_roster = build_roster(&pool, my_picks, facts, week, blend, false);
        let hindsight_roster = build_roster(&pool, my_picks, facts, week, |f| f.actual, false);

        // AI lineup — retry rather than silently degrading into greedy.
        let mut ai = None;
        for attempt in 1..=AI_ATTEMPTS {
            let candidate = lineup::ai_optimize(
                anthropic,
                &ai_roster,
                &settings,
                &[],
                &empty_news,
                args.strategy,
                week,
            )
            .await?;
            if !is_fallback(&candidate.reasoning) {
                ai = Some(candidate);
                break;
            }
            if attempt == AI_ATTEMPTS {
                eprintln!("  week {week}: AI failed {AI_ATTEMPTS}x — {}", candidate.reasoning);
                ai = Some(candidate);
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(20 * attempt as u64)).await;
            }
        }
        let ai = ai.expect("loop always assigns");
        if is_fallback(&ai.reasoning) {
            fallbacks += 1;
        }
        let ai_pts = score(&ai.starters, stats);

        let greedy = lineup::local_optimize(&greedy_roster, &settings, args.strategy, week);
        let greedy_pts = score(&greedy.starters, stats);
        let form_lineup = lineup::local_optimize(&form_roster, &settings, args.strategy, week);
        let form_pts = score(&form_lineup.starters, stats);
        let naive_pts: f32 = naive_ids.iter().map(|id| pts(stats, id)).sum();
        let optimal = lineup::local_optimize(&hindsight_roster, &settings, args.strategy, week);
        let opt_pts = score(&optimal.starters, stats);

        // How far did the AI actually stray from the greedy answer?
        let swaps = starter_ids(&ai).difference(&starter_ids(&greedy)).count() as u32;
        total_swaps += swaps;
        if swaps > 0 {
            weeks_deviated += 1;
        }

        tot_ai += ai_pts;
        tot_greedy += greedy_pts;
        tot_form += form_pts;
        tot_naive += naive_pts;
        tot_opt += opt_pts;
        if ai_pts > greedy_pts {
            ai_beat_greedy += 1;
        } else if ai_pts < greedy_pts {
            ai_lost_greedy += 1;
        }

        let snippet: String = ai.reasoning.chars().take(58).collect();
        println!(
            "{:<5} {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>4}  {}{}",
            week,
            ai_pts,
            greedy_pts,
            form_pts,
            naive_pts,
            opt_pts,
            swaps,
            snippet,
            if is_fallback(&ai.reasoning) { "  [FALLBACK]" } else { "" }
        );
    }

    // -- verdict -------------------------------------------------------------
    let weeks = args.weeks as f32;
    println!("\n===== SEASON TOTALS ({} weeks) =====", args.weeks);
    println!("  AI (Claude):       {:>8.1}  ({:.1}/wk)", tot_ai, tot_ai / weeks);
    println!("  Form blend:        {:>8.1}  ({:.1}/wk)", tot_form, tot_form / weeks);
    println!("  Greedy (proj only):{:>8.1}  ({:.1}/wk)", tot_greedy, tot_greedy / weeks);
    println!("  Naive (set/forget):{:>8.1}  ({:.1}/wk)", tot_naive, tot_naive / weeks);
    println!("  Hindsight optimal: {:>8.1}  ({:.1}/wk)", tot_opt, tot_opt / weeks);
    println!(
        "\n  AI vs greedy:  {:+.1} pts ({:+.1}/wk), won {} / lost {} / tied {} weeks",
        tot_ai - tot_greedy,
        (tot_ai - tot_greedy) / weeks,
        ai_beat_greedy,
        ai_lost_greedy,
        args.weeks as u32 - ai_beat_greedy - ai_lost_greedy
    );
    println!(
        "  AI vs form:    {:+.1} pts ({:+.1}/wk)",
        tot_ai - tot_form,
        (tot_ai - tot_form) / weeks
    );
    println!("  AI vs naive:   {:+.1} pts ({:+.1}/wk)", tot_ai - tot_naive, (tot_ai - tot_naive) / weeks);
    println!(
        "  Headroom the form signal offers: {:+.1} pts (form vs greedy)",
        tot_form - tot_greedy
    );
    println!(
        "  Deviation:     {} starter swaps vs greedy across {}/{} weeks",
        total_swaps, weeks_deviated, args.weeks
    );
    if fallbacks > 0 {
        println!(
            "  !! {fallbacks}/{} weeks fell back to the local optimizer — AI total is contaminated",
            args.weeks
        );
    }
    if tot_opt > 0.0 {
        println!(
            "  Efficiency:    AI {:.1}% of hindsight-optimal (form {:.1}%, greedy {:.1}%, naive {:.1}%)",
            100.0 * tot_ai / tot_opt,
            100.0 * tot_form / tot_opt,
            100.0 * tot_greedy / tot_opt,
            100.0 * tot_naive / tot_opt
        );
    }
    Ok(())
}

/// Projection/trailing-form blend with `w` weight on form; falls back to pure
/// projection until a player has enough games for the form number to mean
/// anything.
fn blend_w(f: &WeekFacts, w: f32) -> f32 {
    match f.form {
        Some(form) => (1.0 - w) * f.proj + w * form,
        None => f.proj,
    }
}

fn blend(f: &WeekFacts) -> f32 {
    blend_w(f, 1.0 - PROJ_WEIGHT)
}
