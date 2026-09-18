//! Waiver analysis, Sleeper-native.
//!
//! Signals used per candidate:
//! - Sleeper weekly projections (real, not heuristic)
//! - League-wide trending adds/drops over the last 24h (community wisdom)
//! - Local PlayerMetrics (floor/ceiling/risk/strategy fit)
//! - Drop candidate at the same position from your roster
//!
//! Claude then re-ranks the shortlist with news + recent league transactions.

use crate::anthropic::Anthropic;
use crate::api::LeagueSession;
use crate::metrics::PlayerMetrics;
use crate::news::NewsItem;
use crate::strategy::Strategy;
use crate::types::*;
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize)]
pub struct WaiverCandidate {
    pub priority: u32,
    pub player: Player,
    pub metrics: PlayerMetrics,
    pub trending_adds: Option<u64>,
    pub drop_candidate: Option<DropCandidate>,
    pub reasoning: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DropCandidate {
    pub player: Player,
    pub metrics: PlayerMetrics,
    pub net_ros_delta: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WaiverReport {
    pub candidates: Vec<WaiverCandidate>,
    pub raw: String,
}

pub async fn analyze(
    session: &LeagueSession,
    anthropic: &Anthropic,
    strategy: Strategy,
    news: &[NewsItem],
    pool: usize,
) -> Result<WaiverReport> {
    let week = session.current_week().await?;
    let roster = session.my_roster(week).await?;
    let free_agents = session.free_agents(None, week, pool).await?;
    let trending = session
        .trending_players(TrendDirection::Add, 50)
        .await
        .unwrap_or_default();
    let trending_counts: HashMap<&str, u64> = trending
        .iter()
        .map(|t| (t.player.id.as_str(), t.count))
        .collect();
    let transactions = session.recent_transactions(week, 2).await.unwrap_or_default();

    let roster_metrics: Vec<PlayerMetrics> = roster
        .players
        .iter()
        .map(|p| PlayerMetrics::for_player(p, strategy))
        .collect();

    // Weakest rostered player per position (drop candidates).
    let mut weakest_at: HashMap<Position, usize> = HashMap::new();
    for (i, m) in roster_metrics.iter().enumerate() {
        if matches!(roster.players[i].roster_slot, Position::IR) {
            continue;
        }
        weakest_at
            .entry(m.position)
            .and_modify(|cur| {
                if m.ros_value < roster_metrics[*cur].ros_value {
                    *cur = i;
                }
            })
            .or_insert(i);
    }

    // Score every free agent: projection upgrade × strategy fit, boosted by
    // trending count (log-scaled so a 10k-add player doesn't drown out signal).
    let mut scored: Vec<(usize, f32, PlayerMetrics)> = free_agents
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let m = PlayerMetrics::for_player(p, strategy);
            // No incumbent at the position (e.g. pre-draft): compare against a
            // conservative replacement baseline instead of crediting the full
            // ROS value — otherwise position holes dominate the shortlist and
            // cross-position scores stop being comparable.
            let replacement_baseline = m.ros_value * 0.6;
            let upgrade = weakest_at
                .get(&m.position)
                .map(|idx| m.ros_value - roster_metrics[*idx].ros_value)
                .unwrap_or(m.ros_value - replacement_baseline);
            let trend_boost = trending_counts
                .get(p.id.as_str())
                .map(|c| 1.0 + (*c as f32).ln_1p() / 10.0)
                .unwrap_or(1.0);
            let score = upgrade * (0.5 + 0.5 * m.strategy_fit) * trend_boost;
            (i, score, m)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let candidates: Vec<WaiverCandidate> = scored
        .into_iter()
        .filter(|(_, score, _)| *score > 0.0)
        .take(8)
        .enumerate()
        .map(|(rank, (idx, score, metrics))| {
            let player = free_agents[idx].clone();
            let drop_candidate = weakest_at.get(&metrics.position).map(|widx| DropCandidate {
                player: roster.players[*widx].clone(),
                metrics: roster_metrics[*widx].clone(),
                net_ros_delta: metrics.ros_value - roster_metrics[*widx].ros_value,
            });
            let trending_adds = trending_counts.get(player.id.as_str()).copied();
            WaiverCandidate {
                priority: (rank + 1) as u32,
                reasoning: format!(
                    "score {:.1}{}",
                    score,
                    trending_adds
                        .map(|c| format!(", {c} adds/24h league-wide"))
                        .unwrap_or_default()
                ),
                player,
                metrics,
                trending_adds,
                drop_candidate,
            }
        })
        .collect();

    // Surface AI failures instead of leaving the analysis silently empty.
    let raw = match ai_polish(anthropic, &roster, &candidates, news, &transactions, strategy).await
    {
        Ok(text) => text,
        Err(e) => format!("(AI analysis unavailable: {e})"),
    };

    Ok(WaiverReport { candidates, raw })
}

async fn ai_polish(
    anthropic: &Anthropic,
    roster: &Roster,
    candidates: &[WaiverCandidate],
    news: &[NewsItem],
    transactions: &[Transaction],
    strategy: Strategy,
) -> Result<String> {
    if candidates.is_empty() {
        return Ok(String::new());
    }
    let system = format!(
        "You are an autonomous fantasy football GM finalizing a waiver report \
         for a Sleeper league. {}\n\
         Re-rank or confirm the candidate list. Consider the trending-add \
         counts (league-wide Sleeper community behavior), your league's recent \
         transactions (what rivals are doing), and the news. End with a \
         2-4 sentence action summary including FAAB bid sizing advice \
         (aggressive/moderate/minimal).",
        strategy.guidance()
    );
    let cand_block = candidates
        .iter()
        .map(|c| {
            let drop = c
                .drop_candidate
                .as_ref()
                .map(|d| format!("drop {} (Δ {:+.0} ROS)", d.player.name, d.net_ros_delta))
                .unwrap_or_else(|| "no drop needed".into());
            format!(
                "{}. {} ({} {}) proj {:.1}/wk ROS {:.0} risk {:.2} fit {:.2}{} | {}",
                c.priority,
                c.player.name,
                c.player.position,
                c.player.team,
                c.metrics.adjusted_next_week,
                c.metrics.ros_value,
                c.metrics.risk_score,
                c.metrics.strategy_fit,
                c.trending_adds
                    .map(|n| format!(" | {n} adds/24h"))
                    .unwrap_or_default(),
                drop
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tx_block = transactions
        .iter()
        .take(12)
        .map(|t| {
            let adds: Vec<String> = t.adds.iter().map(|(p, tm)| format!("{tm} +{p}")).collect();
            let drops: Vec<String> = t.drops.iter().map(|(p, tm)| format!("{tm} -{p}")).collect();
            format!(
                "wk{} {} [{}]: {} {}{}",
                t.week,
                t.kind,
                t.status,
                adds.join(", "),
                drops.join(", "),
                t.waiver_bid.map(|b| format!(" (${b} FAAB)")).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let news_block = news
        .iter()
        .take(10)
        .map(|n| format!("- [{}] {}", n.source, n.title))
        .collect::<Vec<_>>()
        .join("\n");
    let roster_names = roster
        .players
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let user = format!(
        "My roster: {roster_names}\n\n\
         Candidates (metrics + trending):\n{cand_block}\n\n\
         Recent league transactions:\n{}\n\n\
         Recent news:\n{}\n\n\
         Give the final ranked top 5 with one-line rationale each, then the \
         action summary with FAAB advice.",
        if tx_block.is_empty() { "(none)" } else { &tx_block },
        if news_block.is_empty() { "(none)" } else { &news_block },
    );
    anthropic.complete_for(crate::anthropic::AiFeature::Waiver, &system, &user).await
}

/// Ask Claude why a player is suddenly being added or dropped league-wide,
/// and whether it matters for this roster.
///
/// Sleeper reports the raw add/drop counts but never the reason, which is the
/// part that decides whether a spike is an injury backfill worth chasing or
/// noise off a single big game.
pub async fn explain_trending(
    anthropic: &crate::anthropic::Anthropic,
    player: &Player,
    count: u64,
    direction: TrendDirection,
    my_roster: &Roster,
    news: &[NewsItem],
    strat: Strategy,
) -> anyhow::Result<String> {
    let system = format!(
        "You are a fantasy football analyst. Explain concisely why a player is trending. \
         Strategy: {}. Be specific about the likely cause (injury to a team-mate, role \
         change, schedule, a single outlier game) and say plainly when you are not sure. \
         Never invent injuries or transactions that are not supported by the data given.",
        strat.guidance()
    );

    let roster_block = my_roster
        .players
        .iter()
        .map(|p| format!("{} ({} {})", p.name, p.position, p.team))
        .collect::<Vec<_>>()
        .join(", ");

    let news_block = if news.is_empty() {
        "(no recent headlines)".to_string()
    } else {
        news.iter()
            .take(12)
            .map(|n| format!("- {}", n.title))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let verb = match direction {
        TrendDirection::Add => "ADDED",
        TrendDirection::Drop => "DROPPED",
    };

    let user = format!(
        "Player: {name} ({pos} {team}), status {status}, projected {proj:.1} this week.\n\
         Being {verb} by {count} teams league-wide in the last 24 hours.\n\n\
         === RECENT HEADLINES ===\n{news_block}\n\n\
         === MY ROSTER ===\n{roster_block}\n\n\
         Answer in three short sections:\n\
         WHY: the most likely reason for the move, with your confidence.\n\
         OUTLOOK: what to expect from him over the next few weeks.\n\
         FOR ME: whether he improves this specific roster, and who he would replace. \
         Say \"no action\" if he does not.",
        name = player.name,
        pos = player.position,
        team = player.team,
        status = player.status,
        proj = player.projected_points,
    );

    anthropic
        .complete_for(crate::anthropic::AiFeature::Trending, &system, &user)
        .await
}
