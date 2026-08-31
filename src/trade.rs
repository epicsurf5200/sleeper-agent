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

// ---------------------------------------------------------------------------
// Structured suggestions
// ---------------------------------------------------------------------------

/// One leg of a trade. A simple deal is a single step; a multi-tier idea
/// chains several, where a player acquired in step 1 is dealt on in step 2.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TradeStep {
    #[serde(default)]
    pub partner: String,
    #[serde(default)]
    pub send: Vec<String>,
    #[serde(default)]
    pub receive: Vec<String>,
    #[serde(default)]
    pub why: String,
}

/// A complete proposal, possibly spanning several trades.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TradeIdea {
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub steps: Vec<TradeStep>,
    #[serde(default)]
    pub why: String,
    /// What would have to go right, in the model's own words.
    #[serde(default)]
    pub risk: String,
}

impl TradeIdea {
    /// True when this involves more than one trade.
    pub fn is_multi_tier(&self) -> bool {
        self.steps.len() > 1
    }

    /// True when the steps genuinely hand off — some step sends on a player
    /// acquired in an earlier one.
    ///
    /// Asked for a chain, the model will sometimes return two unrelated trades
    /// bundled under a chain-sounding headline. That is a different (and less
    /// fragile) thing than a real chain, so the label is derived from the
    /// steps rather than taken from the model's description.
    pub fn is_chained(&self) -> bool {
        let mut acquired: Vec<&String> = Vec::new();
        for step in &self.steps {
            if step.send.iter().any(|p| acquired.contains(&p)) {
                return true;
            }
            acquired.extend(step.receive.iter());
        }
        false
    }

    /// How to describe the shape of this proposal in the UI.
    pub fn shape_label(&self) -> Option<String> {
        match (self.steps.len(), self.is_chained()) {
            (0 | 1, _) => None,
            (n, true) => Some(format!("{n}-step chain")),
            (n, false) => Some(format!("{n} independent trades")),
        }
    }

    /// Players leaving this roster across every step, minus any that were
    /// acquired earlier in the same chain — those were never ours to lose.
    pub fn net_send(&self) -> Vec<String> {
        let acquired: Vec<&String> = self.steps.iter().flat_map(|s| s.receive.iter()).collect();
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for name in self.steps.iter().flat_map(|s| s.send.iter()) {
            // Only count a player as acquired-then-flipped once.
            if acquired.contains(&name) && !seen.contains(name) {
                seen.push(name.clone());
                continue;
            }
            out.push(name.clone());
        }
        out
    }

    /// Players remaining on this roster once the whole chain completes.
    pub fn net_receive(&self) -> Vec<String> {
        let sent: Vec<&String> = self.steps.iter().flat_map(|s| s.send.iter()).collect();
        let mut out = Vec::new();
        for name in self.steps.iter().flat_map(|s| s.receive.iter()) {
            if sent.contains(&name) {
                continue; // acquired then dealt on
            }
            out.push(name.clone());
        }
        out
    }
}

/// Whether a trade is judged on this week's matchup or the whole season.
///
/// These pull in genuinely different directions: a player on bye is worthless
/// this week and unaffected over a season, and an injured star is the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Horizon {
    ThisWeek,
    RestOfSeason,
}

impl Horizon {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ThisWeek => "This week",
            Self::RestOfSeason => "Rest of season",
        }
    }

    /// Guidance handed to the model.
    fn guidance(&self, week: u8) -> String {
        match self {
            Self::ThisWeek => format!(
                "Judge every deal purely on week {week}. A player on bye or ruled out \
                 this week is worth nothing to me now regardless of his talent, and a \
                 favourable single matchup is worth chasing. Ignore long-term value."
            ),
            Self::RestOfSeason =>
                "Judge every deal on the remainder of the season. A one-week bye or a \
                 soft matchup is close to irrelevant; durability, role security and \
                 remaining schedule are what matter. Prefer the player who helps most \
                 between now and the playoffs."
                    .to_string(),
        }
    }
}

/// Knobs for a suggestion run.
#[derive(Debug, Clone)]
pub struct SuggestOptions {
    pub count: usize,
    /// Allow chained proposals rather than only single trades.
    pub multi_tier: bool,
    /// Players or positions to move on, free text as typed by the user.
    pub send_hints: Vec<String>,
    /// Positions to acquire.
    pub want_positions: Vec<String>,
    /// Time horizon the deals are judged against.
    pub horizon: Horizon,
    /// Week the horizon is anchored to.
    pub week: u8,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        Self {
            count: 3,
            multi_tier: false,
            send_hints: Vec::new(),
            want_positions: Vec::new(),
            horizon: Horizon::RestOfSeason,
            week: 1,
        }
    }
}

/// Ask for trade ideas as JSON so they can be rendered as structured cards
/// rather than a wall of prose. Returns the parsed ideas and the raw reply,
/// so a response that will not parse can still be shown to the user instead
/// of being swallowed.
pub async fn suggest_ideas(
    anthropic: &Anthropic,
    my_roster: &Roster,
    other_rosters: &[Roster],
    strategy: Strategy,
    news: &[NewsItem],
    opts: &SuggestOptions,
) -> Result<(Vec<TradeIdea>, String)> {
    let others: Vec<&Roster> = other_rosters
        .iter()
        .filter(|r| r.team_id != my_roster.team_id)
        .collect();
    if others.is_empty() {
        return Err(anyhow!("no other rosters to trade with"));
    }

    let tier_rule = if opts.multi_tier {
        "Multi-team ideas are explicitly requested, so at least one idea MUST chain \
         two or more trades: acquire a player from one manager and flip him to \
         another in a later step. Put each leg in `steps`, in the order they must \
         happen, and make the hand-off explicit — a player received in one step \
         should be sent in the next. Every extra leg is another manager who has to \
         say yes, so keep chains to two or three legs and say in `risk` what makes \
         the chain fragile."
    } else {
        "Each idea must be a single trade: exactly one entry in `steps`."
    };

    let mut constraints = Vec::new();
    if !opts.send_hints.is_empty() {
        constraints.push(format!(
            "Build the deals around moving these players or positions: {}.",
            opts.send_hints.join(", ")
        ));
    }
    if !opts.want_positions.is_empty() {
        constraints.push(format!(
            "Prioritise acquiring these positions: {}.",
            opts.want_positions.join(", ")
        ));
    }
    let constraint_block = if constraints.is_empty() {
        "Target my weakest starting spot using my deepest surplus.".to_string()
    } else {
        constraints.join(" ")
    };

    let system = format!(
        "You are an autonomous fantasy football GM looking for trades that help \
         my team. {}\n\
         Only propose trades involving players actually listed on the named \
         rosters, and spell every player name exactly as it appears there. \
         Be concrete and realistic — the other manager must plausibly say yes, \
         so do not propose lopsided robbery. {}\n\
         {}\n\
         Reply with JSON only: no prose, no code fences.",
        strategy.guidance(),
        tier_rule,
        opts.horizon.guidance(opts.week)
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
        "=== MY TEAM ({my_team}) ===\n{me}\n\n\
         === OTHER ROSTERS ===\n{league}\n\n\
         === RECENT NEWS ===\n{news}\n\n\
         {constraint_block}\n\
         Horizon: {horizon}.\n\n\
         Return up to {count} ideas as a JSON array. Each element:\n\
         {{\n\
         \x20 \"headline\": \"<short label, e.g. 'Turn RB depth into a WR1'>\",\n\
         \x20 \"steps\": [\n\
         \x20   {{\"partner\": \"<team name>\",\n\
         \x20    \"send\": [\"<my player>\"],\n\
         \x20    \"receive\": [\"<their player>\"],\n\
         \x20    \"why\": \"<one sentence on why they accept>\"}}\n\
         \x20 ],\n\
         \x20 \"why\": \"<two sentences on why this helps me>\",\n\
         \x20 \"risk\": \"<one sentence: what has to go right>\"\n\
         }}\n\n\
         If no trade is worth making, return an empty array: []",
        my_team = my_roster.team_name,
        news = if news_block.is_empty() { "(none)".into() } else { news_block },
        count = opts.count,
        horizon = opts.horizon.label(),
    );

    let raw = anthropic
        .complete_for(crate::anthropic::AiFeature::Trade, &system, &user)
        .await?;
    let ideas = parse_ideas(&raw);
    Ok((ideas, raw))
}

/// Pull the trade array out of a model reply.
///
/// The reply is asked to be bare JSON but in practice can arrive fenced or
/// with a sentence in front, so the first balanced array is extracted rather
/// than parsing the whole string.
pub fn parse_ideas(raw: &str) -> Vec<TradeIdea> {
    let Some(slice) = first_json_array(raw) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<TradeIdea>>(slice)
        .map_err(|e| tracing::warn!(error = %e, "trade ideas did not parse as JSON"))
        .unwrap_or_default()
        .into_iter()
        // An idea with no legs is not actionable and would render as an empty
        // card, so drop it here rather than in the UI.
        .filter(|i| !i.steps.is_empty())
        .collect()
}

/// The first balanced `[...]` in `s`, ignoring brackets inside JSON strings.
fn first_json_array(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let bytes = s.as_bytes();
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_str {
            match c {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod suggest_tests {
    use super::*;

    #[test]
    fn parses_a_bare_json_array() {
        let raw = r#"[{"headline":"H","steps":[{"partner":"P","send":["A"],"receive":["B"],"why":"w"}],"why":"y","risk":"r"}]"#;
        let ideas = parse_ideas(raw);
        assert_eq!(ideas.len(), 1);
        assert_eq!(ideas[0].headline, "H");
        assert_eq!(ideas[0].steps[0].partner, "P");
        assert!(!ideas[0].is_multi_tier());
    }

    #[test]
    fn tolerates_fences_and_a_preamble() {
        let raw = "Sure, here you go:\n```json\n[{\"headline\":\"H\",\"steps\":[{\"partner\":\"P\",\"send\":[\"A\"],\"receive\":[\"B\"]}]}]\n```\nHope that helps!";
        assert_eq!(parse_ideas(raw).len(), 1);
    }

    #[test]
    fn brackets_inside_strings_do_not_end_the_array() {
        let raw = r#"[{"headline":"a ] bracket","steps":[{"partner":"P","send":["A"],"receive":["B"]}]}]"#;
        let ideas = parse_ideas(raw);
        assert_eq!(ideas.len(), 1);
        assert_eq!(ideas[0].headline, "a ] bracket");
    }

    #[test]
    fn empty_array_and_unparseable_text_both_yield_nothing() {
        assert!(parse_ideas("[]").is_empty());
        assert!(parse_ideas("NO ACTION").is_empty());
        assert!(parse_ideas("[not json]").is_empty());
    }

    #[test]
    fn ideas_without_steps_are_dropped() {
        assert!(parse_ideas(r#"[{"headline":"empty","steps":[]}]"#).is_empty());
    }

    #[test]
    fn unrelated_steps_are_not_called_a_chain() {
        // Two trades that share no player: bundled, not chained.
        let idea = TradeIdea {
            steps: vec![
                TradeStep {
                    partner: "A".into(),
                    send: vec!["X".into()],
                    receive: vec!["Y".into()],
                    ..Default::default()
                },
                TradeStep {
                    partner: "B".into(),
                    send: vec!["P".into()],
                    receive: vec!["Q".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(idea.is_multi_tier());
        assert!(!idea.is_chained());
        assert_eq!(idea.shape_label().unwrap(), "2 independent trades");
    }

    #[test]
    fn a_real_hand_off_is_recognised_as_a_chain() {
        let idea = TradeIdea {
            steps: vec![
                TradeStep {
                    partner: "A".into(),
                    send: vec!["Mine".into()],
                    receive: vec!["Middle".into()],
                    ..Default::default()
                },
                TradeStep {
                    partner: "B".into(),
                    send: vec!["Middle".into()],
                    receive: vec!["Target".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(idea.is_chained());
        assert_eq!(idea.shape_label().unwrap(), "2-step chain");
    }

    #[test]
    fn a_single_step_has_no_shape_label() {
        let idea = TradeIdea {
            steps: vec![TradeStep::default()],
            ..Default::default()
        };
        assert!(idea.shape_label().is_none());
    }

    #[test]
    fn net_effect_cancels_a_player_acquired_then_flipped() {
        let idea = TradeIdea {
            steps: vec![
                TradeStep {
                    partner: "A".into(),
                    send: vec!["Mine".into()],
                    receive: vec!["Middle".into()],
                    ..Default::default()
                },
                TradeStep {
                    partner: "B".into(),
                    send: vec!["Middle".into()],
                    receive: vec!["Target".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(idea.is_multi_tier());
        // "Middle" never sticks on either side of the ledger.
        assert_eq!(idea.net_send(), vec!["Mine".to_string()]);
        assert_eq!(idea.net_receive(), vec!["Target".to_string()]);
    }
}
