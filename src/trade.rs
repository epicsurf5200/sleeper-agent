use crate::anthropic::Anthropic;
use crate::metrics::{PackageMetrics, PlayerMetrics};
use crate::news::NewsItem;
use crate::strategy::Strategy;
use crate::types::*;
use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct TradeAnalysis {
    pub send: Vec<PlayerMetrics>,
    pub receive: Vec<PlayerMetrics>,
    pub send_pkg: PackageMetrics,
    pub receive_pkg: PackageMetrics,
    /// Positive = good for us (in ROS points).
    pub net_ros_delta: f32,
    /// 0..1 — 0.5 is fair; >0.5 is favorable to us.
    pub fairness: f32,
    /// "ACCEPT" / "DECLINE" / "NEGOTIATE"
    pub verdict: &'static str,
    pub ai_summary: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn analyze(
    anthropic: &Anthropic,
    my_roster: &Roster,
    partner_team: &str,
    send_names: &[String],
    receive_names: &[String],
    other_rosters: &[Roster],
    strategy: Strategy,
    news: &[NewsItem],
) -> Result<TradeAnalysis> {
    let send = resolve_players(my_roster, send_names)
        .map_err(|missing| anyhow!("send player not found on your roster: {missing}"))?;
    // `other_rosters` typically includes our own team — never trade with ourselves.
    let partner = other_rosters
        .iter()
        .filter(|r| r.team_id != my_roster.team_id)
        .find(|r| r.team_name.eq_ignore_ascii_case(partner_team))
        .or_else(|| {
            // fall back: any team containing the receive players
            other_rosters
                .iter()
                .filter(|r| r.team_id != my_roster.team_id)
                .find(|r| {
                    receive_names.iter().all(|n| {
                        r.players
                            .iter()
                            .any(|p| p.name.eq_ignore_ascii_case(n))
                    })
                })
        })
        .ok_or_else(|| anyhow!("partner team '{partner_team}' not found"))?;
    let receive = resolve_players(partner, receive_names).map_err(|missing| {
        anyhow!("receive player '{missing}' not on '{}'s roster", partner.team_name)
    })?;

    let send_metrics: Vec<PlayerMetrics> = send
        .iter()
        .map(|p| PlayerMetrics::for_player(p, strategy))
        .collect();
    let receive_metrics: Vec<PlayerMetrics> = receive
        .iter()
        .map(|p| PlayerMetrics::for_player(p, strategy))
        .collect();
    let send_pkg = PackageMetrics::from(&send_metrics);
    let receive_pkg = PackageMetrics::from(&receive_metrics);
    let net = receive_pkg.total_ros - send_pkg.total_ros;
    let fairness = {
        let total = (receive_pkg.total_ros + send_pkg.total_ros).max(1.0);
        ((receive_pkg.total_ros / total) as f32).clamp(0.0, 1.0)
    };
    let verdict = if net > 15.0 && fairness >= 0.55 {
        "ACCEPT"
    } else if net < -15.0 || fairness < 0.40 {
        "DECLINE"
    } else {
        "NEGOTIATE"
    };

    let ai_summary = ai_summary(
        anthropic,
        my_roster,
        partner,
        &send_metrics,
        &receive_metrics,
        &send_pkg,
        &receive_pkg,
        net,
        verdict,
        strategy,
        news,
    )
    .await
    // Surface AI failures instead of leaving the summary silently empty.
    .unwrap_or_else(|e| format!("(AI analysis unavailable: {e})"));

    Ok(TradeAnalysis {
        send: send_metrics,
        receive: receive_metrics,
        send_pkg,
        receive_pkg,
        net_ros_delta: net,
        fairness,
        verdict,
        ai_summary,
    })
}

/// Propose trades rather than grade one. `analyze` needs you to already know
/// what you want; the daemon does not, so this scans the league for
/// complementary surpluses and asks Claude for concrete packages.
pub async fn suggest(
    anthropic: &Anthropic,
    my_roster: &Roster,
    other_rosters: &[Roster],
    strategy: Strategy,
    news: &[NewsItem],
    max_ideas: usize,
) -> Result<String> {
    let others: Vec<&Roster> = other_rosters
        .iter()
        .filter(|r| r.team_id != my_roster.team_id)
        .collect();
    if others.is_empty() {
        return Err(anyhow!("no other rosters to trade with"));
    }

    let system = format!(
        "You are an autonomous fantasy football GM looking for trades that help \
         my team. {}\n\
         Only propose trades involving players actually listed on the named \
         rosters. Prefer 2-for-1 or 1-for-1 deals that convert surplus depth \
         into a starting upgrade. Be concrete and realistic — the other manager \
         must plausibly say yes, so do not propose lopsided robbery.",
        strategy.guidance()
    );

    let me = roster_block(my_roster, strategy);
    let league = others
        .iter()
        .map(|r| roster_block(r, strategy))
        .collect::<Vec<_>>()
        .join("\n\n");
    let news_block = news
        .iter()
        .take(12)
        .map(|n| format!("- [{}] {}", n.source, n.title))
        .collect::<Vec<_>>()
        .join("\n");

    let user = format!(
        "=== MY TEAM ===\n{me}\n\n\
         === OTHER ROSTERS ===\n{league}\n\n\
         === RECENT NEWS ===\n{news}\n\n\
         Identify my weakest starting spot and my deepest surplus, then propose \
         up to {max_ideas} trades that fix the weakness. For each, use exactly:\n\
         TRADE: <partner team>\n\
         SEND: <my player(s), comma separated>\n\
         RECEIVE: <their player(s), comma separated>\n\
         WHY: <two sentences — why it helps me and why they would accept>\n\n\
         If no trade is clearly worth making, reply with exactly: NO ACTION",
        news = if news_block.is_empty() { "(none)".into() } else { news_block },
    );
    anthropic.complete_for(crate::anthropic::AiFeature::Trade, &system, &user).await
}

fn roster_block(roster: &Roster, strategy: Strategy) -> String {
    let players = roster
        .players
        .iter()
        .map(|p| format!("  - {}", PlayerMetrics::for_player(p, strategy).one_line()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{} ({}-{}-{}, PF {:.0}):\n{}",
        roster.team_name, roster.wins, roster.losses, roster.ties, roster.points_for, players
    )
}

fn resolve_players(roster: &Roster, names: &[String]) -> Result<Vec<Player>, String> {
    let mut out = Vec::new();
    for name in names {
        let needle = name.to_lowercase();
        if needle.is_empty() {
            // An empty needle would substring-match the first roster player.
            return Err(name.clone());
        }
        let found = roster.players.iter().find(|p| {
            p.name.eq_ignore_ascii_case(name)
                || p.name.to_lowercase().contains(&needle)
                || (needle.len() >= 4 && needle.contains(&p.name.to_lowercase()))
        });
        match found {
            Some(p) => out.push(p.clone()),
            None => return Err(name.clone()),
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn ai_summary(
    anthropic: &Anthropic,
    my_roster: &Roster,
    partner: &Roster,
    send: &[PlayerMetrics],
    receive: &[PlayerMetrics],
    send_pkg: &PackageMetrics,
    receive_pkg: &PackageMetrics,
    net: f32,
    verdict: &str,
    strategy: Strategy,
    news: &[NewsItem],
) -> Result<String> {
    let system = format!(
        "You are an autonomous fantasy football GM evaluating a trade. {}\n\
         Use the provided per-player metrics. Account for positional needs and \
         roster construction (e.g. RB depth surplus). End with: VERDICT: ACCEPT|DECLINE|NEGOTIATE.",
        strategy.guidance()
    );
    let send_block = send
        .iter()
        .map(|m| format!("  - {}", m.one_line()))
        .collect::<Vec<_>>()
        .join("\n");
    let recv_block = receive
        .iter()
        .map(|m| format!("  - {}", m.one_line()))
        .collect::<Vec<_>>()
        .join("\n");
    let my_pos_counts = position_counts(my_roster);
    let partner_pos_counts = position_counts(partner);
    let news_block = news
        .iter()
        .take(10)
        .filter(|n| {
            let blob = format!("{} {}", n.title, n.summary).to_lowercase();
            send.iter()
                .chain(receive.iter())
                .any(|m| blob.contains(&m.player_name.to_lowercase()))
        })
        .map(|n| format!("- [{}] {}", n.source, n.title))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "My team: {}\nPartner: {}\n\n\
         I SEND (ROS {:.0}, mean {:.1}, ceiling {:.1}, avg risk {:.2}):\n{}\n\n\
         I RECEIVE (ROS {:.0}, mean {:.1}, ceiling {:.1}, avg risk {:.2}):\n{}\n\n\
         Net ROS delta (receive - send): {:+.1}\n\
         Local fairness verdict: {}\n\n\
         My position counts: {:?}\nPartner position counts: {:?}\n\n\
         Recent player news:\n{}\n\n\
         In 4-6 sentences: explain the impact on each side, call out positional \
         fit/scarcity, mention any news that affects valuation, and conclude \
         with a final VERDICT line.",
        my_roster.team_name, partner.team_name,
        send_pkg.total_ros, send_pkg.total_mean, send_pkg.total_ceiling, send_pkg.avg_risk, send_block,
        receive_pkg.total_ros, receive_pkg.total_mean, receive_pkg.total_ceiling, receive_pkg.avg_risk, recv_block,
        net, verdict,
        my_pos_counts, partner_pos_counts,
        if news_block.is_empty() { "(none)".into() } else { news_block },
    );
    anthropic.complete_for(crate::anthropic::AiFeature::Trade, &system, &user).await
}

fn position_counts(roster: &Roster) -> Vec<(Position, u32)> {
    let mut map = std::collections::HashMap::new();
    for p in &roster.players {
        *map.entry(p.position).or_insert(0u32) += 1;
    }
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    v
}
