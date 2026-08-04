use crate::anthropic::Anthropic;
use crate::news::NewsItem;
use crate::strategy::Strategy;
use crate::types::*;
use anyhow::Result;
use std::collections::HashMap;

/// Adjust a player's projected points based on injury status and chosen strategy.
/// `week` is the target week — a player on bye scores 0 no matter what.
pub fn risk_adjusted(player: &Player, strat: Strategy, week: u8) -> f32 {
    if week > 0 && player.bye_week == Some(week) {
        return 0.0;
    }
    let base = if player.projected_points > 0.0 {
        player.projected_points
    } else {
        player.avg_points
    };
    let mult = match player.status {
        PlayerStatus::Healthy => 1.0,
        PlayerStatus::Questionable => strat.questionable_multiplier(),
        PlayerStatus::Doubtful => strat.doubtful_multiplier(),
        PlayerStatus::Out | PlayerStatus::IR | PlayerStatus::Suspended => 0.0,
        PlayerStatus::Unknown => 0.95,
    };
    base * mult
}

/// Greedy local optimizer that fills required slots in order, using
/// risk-adjusted projections. Used as a deterministic fallback when the
/// AI call fails or as a baseline shown alongside the AI recommendation.
pub fn local_optimize(
    roster: &Roster,
    settings: &LeagueSettings,
    strat: Strategy,
    week: u8,
) -> Lineup {
    let mut available: Vec<&Player> = roster.players.iter().collect();
    available.sort_by(|a, b| {
        risk_adjusted(b, strat, week)
            .partial_cmp(&risk_adjusted(a, strat, week))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut starters: Vec<LineupSlot> = Vec::new();
    let mut chosen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Fill non-flex slots first so flex doesn't grab a top RB and starve RB1.
    let priority = [
        Position::QB,
        Position::RB,
        Position::WR,
        Position::TE,
        Position::K,
        Position::DST,
        Position::FLEX,
        Position::SUPERFLEX,
    ];
    for slot in priority {
        let count = settings
            .roster_slots
            .iter()
            .find_map(|(p, c)| if *p == slot { Some(*c) } else { None })
            .unwrap_or(0);
        for _ in 0..count {
            if let Some((idx, _)) = available
                .iter()
                .enumerate()
                .find(|(_, p)| !chosen_ids.contains(&p.id) && p.eligible_for(slot))
            {
                let p = available[idx].clone();
                chosen_ids.insert(p.id.clone());
                starters.push(LineupSlot {
                    slot,
                    player: Some(p),
                });
            } else {
                starters.push(LineupSlot { slot, player: None });
            }
        }
    }

    let bench: Vec<Player> = roster
        .players
        .iter()
        .filter(|p| !chosen_ids.contains(&p.id))
        .cloned()
        .collect();

    let projected_total: f32 = starters
        .iter()
        .filter_map(|s| s.player.as_ref().map(|p| risk_adjusted(p, strat, week)))
        .sum();

    Lineup {
        week,
        starters,
        bench,
        projected_total,
        reasoning: format!(
            "Greedy {} fallback — slots filled in priority order by risk-adjusted projection.",
            strat.label()
        ),
    }
}

/// Ask Claude to refine the local recommendation with news context. Returns
/// a (Lineup, raw_reasoning) pair. On API failure, the local optimizer's
/// result is returned with the error noted in reasoning.
pub async fn ai_optimize(
    client: &Anthropic,
    roster: &Roster,
    settings: &LeagueSettings,
    matchups: &[Matchup],
    news: &[NewsItem],
    strat: Strategy,
    week: u8,
) -> Result<Lineup> {
    let local = local_optimize(roster, settings, strat, week);

    let system = format!(
        "You are an autonomous fantasy football manager. {}\n\
         You will return a starting lineup, a one-paragraph rationale, and per-slot \
         picks. Use the league settings, the current roster, and any provided news \
         strictly. Do not hallucinate players who are not on the roster.",
        strat.guidance()
    );

    let news_block = if news.is_empty() {
        String::from("(no relevant news)")
    } else {
        news.iter()
            .take(15)
            .map(|n| format!("- [{}] {}", n.source, n.title))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let roster_block = roster
        .players
        .iter()
        .map(|p| {
            let bye = if p.bye_week == Some(week) { "  [ON BYE]" } else { "" };
            let form = if p.avg_points > 0.0 {
                format!(" form={:.1}", p.avg_points)
            } else {
                String::new()
            };
            format!(
                "- {} ({} {}) status={} proj={:.1}{}{}",
                p.name, p.position, p.team, p.status, p.projected_points, form, bye
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let slot_block = settings
        .roster_slots
        .iter()
        .filter(|(p, _)| p.is_starter_slot())
        .map(|(p, c)| format!("{}x{}", p, c))
        .collect::<Vec<_>>()
        .join(", ");

    let matchup_block = matchups
        .iter()
        .find(|m| m.home_team == roster.team_name || m.away_team == roster.team_name)
        .map(|m| {
            format!(
                "Week {} matchup: {} ({:.1} proj) vs {} ({:.1} proj)",
                m.week, m.home_team, m.home_projected, m.away_team, m.away_projected
            )
        })
        .unwrap_or_else(|| format!("Week {week}"));

    let user = format!(
        "Required starting slots: {slot_block}\n\
         Scoring: {scoring}\n\
         {matchup_block}\n\n\
         === ROSTER ===\n{roster_block}\n\n\
         === RECENT NEWS ===\n{news_block}\n\n\
         === LOCAL BASELINE (greedy by risk-adjusted projection) ===\n{baseline}\n\n\
         The baseline ranks by this week's consensus projection ALONE. You also see \
         `form` (trailing actual points per game this season), bye flags, injury \
         status and any news above. Consensus projections already price in most of \
         what `form` reflects, and small-sample form is noisy — so override the \
         baseline only when you have a concrete reason (a bye, an injury, a clear \
         role change), not merely because a bench player has been hot. An unforced \
         swap that does not pay off is a real cost.\n\n\
         Respond in this exact format:\n\
         REASONING: <one paragraph>\n\
         LINEUP:\n\
         <SLOT>: <Player Name>\n\
         (one line per starter slot in the order given above; bench players omitted)",
        scoring = settings.scoring,
        baseline = format_lineup(&local),
    );

    // An AI lineup that fields fewer players than the greedy baseline is
    // strictly worse — an unfilled starter slot forfeits those points. Treat
    // it as a parse failure rather than quietly starting nobody.
    let min_filled = local.starters.iter().filter(|s| s.player.is_some()).count();

    match client.complete(&system, &user).await {
        Ok(text) => Ok(parse_ai_lineup(&text, roster, settings, week, strat, min_filled)
            .unwrap_or_else(|e| {
                tracing::warn!(error=%e, "failed to parse AI lineup, using local fallback");
                Lineup {
                    reasoning: format!("AI parse failed ({e}); fell back to local optimizer."),
                    ..local
                }
            })),
        Err(e) => Ok(Lineup {
            reasoning: format!("AI unavailable ({e}); using local optimizer."),
            ..local
        }),
    }
}

fn format_lineup(l: &Lineup) -> String {
    l.starters
        .iter()
        .map(|s| {
            format!(
                "{}: {}",
                s.slot,
                s.player
                    .as_ref()
                    .map(|p| p.name.as_str())
                    .unwrap_or("(empty)")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_ai_lineup(
    text: &str,
    roster: &Roster,
    settings: &LeagueSettings,
    week: u8,
    strat: Strategy,
    min_filled: usize,
) -> Result<Lineup> {
    let mut reasoning = String::new();
    let mut slot_assignments: Vec<(Position, String)> = Vec::new();
    let mut in_lineup = false;

    for line in text.lines() {
        let trimmed = line.trim();
        // Tolerate markdown decoration around headers: "**REASONING:**", "## Lineup:".
        let stripped = trimmed
            .trim_start_matches(['#', '*', '-', '•', ' '])
            .trim_end_matches('*');
        let upper = stripped.to_uppercase();
        if upper.starts_with("REASONING:") {
            reasoning = stripped[stripped.find(':').map(|i| i + 1).unwrap_or(0)..]
                .trim()
                .trim_matches('*')
                .to_string();
            continue;
        }
        if upper.starts_with("LINEUP") {
            in_lineup = true;
            continue;
        }
        if !in_lineup {
            if !reasoning.is_empty() {
                reasoning.push(' ');
                reasoning.push_str(trimmed);
            }
            continue;
        }
        if let Some((slot_str, name)) = stripped.split_once(':') {
            // Sanitize the slot token: strip list markers ("- RB", "1. WR"),
            // markdown ("**RB**") and positional numbering ("RB1" → RB).
            let slot_clean = slot_str
                .trim()
                .trim_start_matches(|c: char| {
                    c.is_ascii_digit() || matches!(c, '-' | '•' | '*' | '.' | ')' | ' ')
                })
                .trim_matches('*')
                .trim();
            let slot_clean = slot_clean.trim_end_matches(|c: char| c.is_ascii_digit());
            let slot = Position::from_str(slot_clean);
            if slot != Position::Unknown {
                let name = name.trim().trim_matches('*').trim().to_string();
                slot_assignments.push((slot, name));
            }
        }
    }

    if slot_assignments.is_empty() {
        anyhow::bail!("could not parse any slot lines from AI output");
    }

    let by_name: HashMap<String, &Player> = roster
        .players
        .iter()
        .map(|p| (p.name.to_lowercase(), p))
        .collect();

    let mut chosen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut starters = Vec::new();
    for (slot, name) in slot_assignments {
        let needle = name.to_lowercase();
        // An empty/near-empty needle would substring-match an arbitrary player.
        let resolved = if needle.is_empty() || needle == "(empty)" || needle == "none" {
            None
        } else {
            by_name.get(&needle).copied().or_else(|| {
                if needle.len() < 4 {
                    return None; // too short for a safe fuzzy match
                }
                roster.players.iter().find(|p| {
                    p.name.to_lowercase().contains(&needle)
                        || needle.contains(&p.name.to_lowercase())
                })
            })
        };
        if let Some(p) = resolved {
            if !chosen.insert(p.id.clone()) {
                continue;
            }
            starters.push(LineupSlot {
                slot,
                player: Some(p.clone()),
            });
        } else {
            starters.push(LineupSlot { slot, player: None });
        }
    }

    let projected_total: f32 = starters
        .iter()
        .filter_map(|s| s.player.as_ref().map(|p| risk_adjusted(p, strat, week)))
        .sum();
    let bench: Vec<Player> = roster
        .players
        .iter()
        .filter(|p| !chosen.contains(&p.id))
        .cloned()
        .collect();

    // Sanity-check: counts must match required slots, otherwise prefer local optimizer.
    let mut required: HashMap<Position, u32> = HashMap::new();
    for (p, c) in &settings.roster_slots {
        if p.is_starter_slot() {
            required.insert(*p, *c);
        }
    }
    let mut got: HashMap<Position, u32> = HashMap::new();
    for s in &starters {
        *got.entry(s.slot).or_insert(0) += 1;
    }
    if got != required {
        anyhow::bail!(
            "slot counts don't match (got {:?}, required {:?})",
            got,
            required
        );
    }

    let filled = starters.iter().filter(|s| s.player.is_some()).count();
    if filled < min_filled {
        anyhow::bail!(
            "AI left {} starter slot(s) unfilled (fielded {filled}, baseline fields {min_filled})",
            min_filled - filled
        );
    }

    Ok(Lineup {
        week,
        starters,
        bench,
        projected_total,
        reasoning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(name: &str, pos: Position, proj: f32) -> Player {
        Player {
            id: name.to_lowercase().replace(' ', "_"),
            name: name.to_string(),
            position: pos,
            roster_slot: Position::BENCH,
            team: "SF".into(),
            projected_points: proj,
            avg_points: 0.0,
            status: PlayerStatus::Healthy,
            opponent: None,
            bye_week: None,
            news: vec![],
        }
    }

    fn fixture() -> (Roster, LeagueSettings) {
        let roster = Roster {
            team_id: "1".into(),
            team_name: "Testers".into(),
            owner: None,
            players: vec![
                player("Josh Allen", Position::QB, 22.0),
                player("Bijan Robinson", Position::RB, 18.0),
                player("Amon-Ra St. Brown", Position::WR, 16.0),
                player("Sam LaPorta", Position::TE, 11.0),
            ],
            wins: 0,
            losses: 0,
            ties: 0,
            points_for: 0.0,
            points_against: 0.0,
        };
        let settings = LeagueSettings {
            scoring: "ppr".into(),
            roster_slots: vec![
                (Position::QB, 1),
                (Position::RB, 1),
                (Position::WR, 1),
                (Position::TE, 1),
            ],
            team_count: 12,
        };
        (roster, settings)
    }

    #[test]
    fn parses_plain_format() {
        let (roster, settings) = fixture();
        let text = "REASONING: start the studs.\nLINEUP:\nQB: Josh Allen\nRB: Bijan Robinson\nWR: Amon-Ra St. Brown\nTE: Sam LaPorta";
        let l = parse_ai_lineup(text, &roster, &settings, 1, Strategy::Balanced, 4).unwrap();
        assert_eq!(l.starters.len(), 4);
        assert!(l.starters.iter().all(|s| s.player.is_some()));
        assert_eq!(l.reasoning, "start the studs.");
    }

    #[test]
    fn parses_markdown_decorated_format() {
        let (roster, settings) = fixture();
        // Markdown headers, bullets, bold slots, and RB1-style numbering.
        let text = "**REASONING:** upside plays.\n**LINEUP:**\n- **QB:** Josh Allen\n- RB1: Bijan Robinson\n* WR1: Amon-Ra St. Brown\n- TE: Sam LaPorta";
        let l = parse_ai_lineup(text, &roster, &settings, 1, Strategy::Balanced, 4).unwrap();
        assert_eq!(l.starters.len(), 4);
        assert!(l.starters.iter().all(|s| s.player.is_some()), "{:?}", l.starters);
    }

    #[test]
    fn empty_name_does_not_match_arbitrary_player() {
        let (roster, settings) = fixture();
        let text = "REASONING: x\nLINEUP:\nQB: Josh Allen\nRB: Bijan Robinson\nWR:\nTE: Sam LaPorta";
        // min_filled=0 isolates name resolution from the unfilled-slot guard.
        let l = parse_ai_lineup(text, &roster, &settings, 1, Strategy::Balanced, 0).unwrap();
        let wr = l.starters.iter().find(|s| s.slot == Position::WR).unwrap();
        assert!(wr.player.is_none(), "empty WR name must resolve to no player");
    }

    #[test]
    fn unfilled_starter_slot_is_rejected() {
        let (roster, settings) = fixture();
        // An unresolvable name forfeits a starter slot — strictly worse than
        // the greedy baseline, which fields all four. Must not be accepted.
        let text =
            "REASONING: x\nLINEUP:\nQB: Josh Allen\nRB: Bijan Robinson\nWR: Nonexistent Guy\nTE: Sam LaPorta";
        let err = parse_ai_lineup(text, &roster, &settings, 1, Strategy::Balanced, 4)
            .expect_err("lineup fielding 3 of 4 slots must be rejected");
        assert!(err.to_string().contains("unfilled"), "got: {err}");
    }

    #[test]
    fn bye_week_player_scores_zero() {
        let mut p = player("Bijan Robinson", Position::RB, 18.0);
        p.bye_week = Some(5);
        assert_eq!(risk_adjusted(&p, Strategy::Balanced, 5), 0.0);
        assert!(risk_adjusted(&p, Strategy::Balanced, 6) > 0.0);
    }
}
