use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Conservative,
    #[default]
    Balanced,
    HighStakes,
}

impl FromStr for Strategy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace([' ', '-'], "_").as_str() {
            "conservative" | "safe" | "low_risk" => Ok(Self::Conservative),
            "balanced" | "neutral" | "default" => Ok(Self::Balanced),
            "high_stakes" | "high_stakes_high_rewards" | "boom_or_bust" | "aggressive"
            | "high_risk" | "hshr" => Ok(Self::HighStakes),
            other => Err(format!("unknown strategy: {other}")),
        }
    }
}

impl Strategy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Conservative => "Conservative",
            Self::Balanced => "Balanced",
            Self::HighStakes => "High Stakes / High Rewards",
        }
    }

    /// Guidance text injected into Claude prompts.
    pub fn guidance(&self) -> &'static str {
        match self {
            Self::Conservative => {
                "STRATEGY: CONSERVATIVE. Prioritize floor over ceiling. Prefer healthy, \
                 high-snap-share veterans with reliable target volume or carry share. \
                 Avoid players listed as Questionable/Doubtful unless no alternative exists. \
                 Avoid boom-or-bust deep threats and TD-dependent players. Favor matchups \
                 against bottom-10 defenses by points allowed to position. Tiebreakers go \
                 to the player with the steadier weekly variance."
            }
            Self::Balanced => {
                "STRATEGY: BALANCED. Optimize expected points using projections, recent form \
                 (last 3-4 weeks), and matchup. Treat injury designations as risk-adjusted \
                 (Q ~85% of projection, D ~50%, OUT 0). Mix floor and ceiling — start the \
                 player with the higher mean projection unless variance is extreme. Use \
                 FLEX on the highest-EV eligible player regardless of position."
            }
            Self::HighStakes => {
                "STRATEGY: HIGH STAKES, HIGH REWARDS. Maximize ceiling. Lean into players \
                 with breakout matchups, vacated targets, goal-line work, or shootout game \
                 environments (high Vegas totals). Tolerate Questionable tags for players \
                 with elite upside. Prefer the boom-or-bust WR over the steady possession \
                 receiver. Stack QB with a pass-catcher when a tournament-style edge exists. \
                 Tiebreakers go to the higher-ceiling player."
            }
        }
    }

    /// Risk multiplier applied to a Questionable tag in the local optimizer fallback.
    pub fn questionable_multiplier(&self) -> f32 {
        match self {
            Self::Conservative => 0.55,
            Self::Balanced => 0.85,
            Self::HighStakes => 0.95,
        }
    }

    pub fn doubtful_multiplier(&self) -> f32 {
        match self {
            Self::Conservative => 0.15,
            Self::Balanced => 0.50,
            Self::HighStakes => 0.65,
        }
    }
}
