//! Draft manager, Sleeper-native. Sleeper exposes the live draft feed and
//! the draft order, so we can tell exactly when you're on the clock.

use crate::anthropic::Anthropic;
use crate::api::LeagueSession;
use crate::news::NewsItem;
use crate::strategy::Strategy;
use crate::types::*;
use anyhow::Result;
use std::collections::HashSet;

pub struct DraftManager<'a> {
    pub session: &'a LeagueSession,
    pub anthropic: &'a Anthropic,
    pub strategy: Strategy,
    pub my_team_name: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DraftSuggestion {
    pub picks: Vec<SuggestedPick>,
    pub raw: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SuggestedPick {
    pub rank: u32,
    pub name: String,
    pub position: Option<Position>,
    pub rationale: String,
}

impl<'a> DraftManager<'a> {
    pub async fn snapshot(&self) -> Result<DraftState> {
        self.session.draft_state().await
    }

    pub fn is_my_turn(&self, state: &DraftState) -> bool {
        // Prefer the user_id: team names can be customized, so display-name
        // comparison fails for any manager with a custom team name.
        if let Some(uid) = state.on_the_clock_user_id.as_deref() {
            return uid == self.session.my_user_id;
        }
        state
            .on_the_clock_team
            .as_deref()
            .map(|t| t.eq_ignore_ascii_case(&self.my_team_name))
            .unwrap_or(false)
    }

    pub async fn ask_claude(
        &self,
        state: &DraftState,
        news: &[NewsItem],
    ) -> Result<DraftSuggestion> {
        let system = format!(
            "You are an autonomous fantasy football draft assistant for a \
             Sleeper league. {}\n\
             Recommend exactly 3 players for the next pick. Respect positional \
             needs, bye-week distribution, handcuffs/sleepers, and the strategy \
             guidance. Do NOT recommend any player already drafted.",
            self.strategy.guidance()
        );

        let drafted: HashSet<String> = state
            .picks
            .iter()
            .filter_map(|p| p.player_name.as_ref().map(|n| n.to_lowercase()))
            .collect();

        let my_picks: Vec<&DraftPick> = state
            .picks
            .iter()
            .filter(|p| p.team_name.eq_ignore_ascii_case(&self.my_team_name))
            .collect();
        let my_block = if my_picks.is_empty() {
            "(no picks yet)".to_string()
        } else {
            my_picks
                .iter()
                .map(|p| {
                    format!(
                        "  R{}.{} {}",
                        p.round,
                        p.pick_number,
                        p.player_name.as_deref().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let recent = state
            .picks
            .iter()
            .rev()
            .take(24)
            .rev()
            .map(|p| {
                format!(
                    "  R{}.{} {} -> {} ({})",
                    p.round,
                    p.pick_number,
                    p.team_name,
                    p.player_name.as_deref().unwrap_or("?"),
                    p.position.map(|x| x.to_string()).unwrap_or_default(),
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

        let user = format!(
            "Current pick: overall #{} (round {} of {}). {} teams.\n\
             My team: {}\n\
             My picks so far:\n{}\n\n\
             Recent picks:\n{}\n\n\
             Recent news:\n{}\n\n\
             Respond in this exact format (3 entries):\n\
             1. <Name> (<POS>) — <one-line rationale>\n\
             2. <Name> (<POS>) — <one-line rationale>\n\
             3. <Name> (<POS>) — <one-line rationale>",
            state.current_pick,
            ((state.current_pick.saturating_sub(1) / state.team_count.max(1)) + 1),
            state.total_rounds,
            state.team_count,
            self.my_team_name,
            my_block,
            recent,
            if news_block.is_empty() { "(none)" } else { &news_block },
        );

        let raw = self.anthropic.complete_for(crate::anthropic::AiFeature::Draft, &system, &user).await?;
        // Belt-and-suspenders: never surface a player who's already off the board.
        let picks = parse_suggestions(&raw)
            .into_iter()
            .filter(|s| !drafted.contains(&s.name.to_lowercase()))
            .collect();
        Ok(DraftSuggestion { picks, raw })
    }
}

fn parse_suggestions(text: &str) -> Vec<SuggestedPick> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .strip_prefix("1.")
            .or_else(|| trimmed.strip_prefix("2."))
            .or_else(|| trimmed.strip_prefix("3."))
        else {
            continue;
        };
        let rank = trimmed
            .chars()
            .next()
            .and_then(|c| c.to_digit(10))
            .unwrap_or(0);
        let rest = rest.trim();
        // Split name from rationale on a *space-delimited* dash only — a bare
        // '-' split would cut hyphenated names like "Amon-Ra St. Brown".
        let (name_part, rationale) = match rest
            .split_once(" — ")
            .or_else(|| rest.split_once(" – "))
            .or_else(|| rest.split_once(" - "))
            .or_else(|| rest.split_once('—'))
        {
            Some((n, r)) => (n.trim().to_string(), r.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
        let (name, position) = match name_part.rsplit_once('(') {
            Some((n, p)) => {
                let pos = Position::from_str(p.trim_end_matches(')').trim());
                (
                    n.trim().to_string(),
                    if pos == Position::Unknown { None } else { Some(pos) },
                )
            }
            None => (name_part, None),
        };
        out.push(SuggestedPick {
            rank,
            name,
            position,
            rationale,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphenated_names_survive_plain_dash_separator() {
        let picks =
            parse_suggestions("1. Amon-Ra St. Brown (WR) - elite target share\n2. Ja'Marr Chase (WR) — WR1 upside");
        assert_eq!(picks.len(), 2);
        assert_eq!(picks[0].name, "Amon-Ra St. Brown");
        assert_eq!(picks[0].rationale, "elite target share");
        assert_eq!(picks[1].name, "Ja'Marr Chase");
    }
}
