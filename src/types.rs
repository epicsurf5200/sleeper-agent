use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Position {
    QB,
    RB,
    WR,
    TE,
    K,
    DST,
    FLEX,
    SUPERFLEX,
    BENCH,
    IR,
    Unknown,
}

impl Position {
    #[allow(clippy::should_implement_trait)] // infallible by design, unlike FromStr
    pub fn from_str(s: &str) -> Self {
        match s.to_uppercase().replace('/', "").as_str() {
            "QB" => Self::QB,
            "RB" => Self::RB,
            "WR" => Self::WR,
            "TE" => Self::TE,
            "K" | "PK" => Self::K,
            "D" | "DEF" | "DST" | "DSTDEF" => Self::DST,
            "FLEX" | "RBWRTE" | "WRT" | "WRRBTE" => Self::FLEX,
            // "SFLEX" is what our own Display emits — must round-trip.
            "SUPERFLEX" | "SFLEX" | "SFLX" | "SUPER_FLEX" | "OP" => Self::SUPERFLEX,
            "BE" | "BN" | "BENCH" => Self::BENCH,
            "IR" => Self::IR,
            _ => Self::Unknown,
        }
    }

    pub fn is_starter_slot(&self) -> bool {
        !matches!(self, Self::BENCH | Self::IR | Self::Unknown)
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::QB => "QB",
            Self::RB => "RB",
            Self::WR => "WR",
            Self::TE => "TE",
            Self::K => "K",
            Self::DST => "DST",
            Self::FLEX => "FLEX",
            Self::SUPERFLEX => "SFLEX",
            Self::BENCH => "BE",
            Self::IR => "IR",
            Self::Unknown => "?",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerStatus {
    Healthy,
    Questionable,
    Doubtful,
    Out,
    IR,
    Suspended,
    Unknown,
}

impl PlayerStatus {
    #[allow(clippy::should_implement_trait)] // infallible by design, unlike FromStr
    pub fn from_str(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "ACTIVE" | "HEALTHY" | "" | "NORMAL" => Self::Healthy,
            "QUESTIONABLE" | "Q" => Self::Questionable,
            "DOUBTFUL" | "D" => Self::Doubtful,
            "OUT" | "O" => Self::Out,
            "IR" | "INJURY_RESERVE" | "INJURED_RESERVE" => Self::IR,
            "SUSPENDED" | "SUSP" => Self::Suspended,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for PlayerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Healthy => "OK",
            Self::Questionable => "Q",
            Self::Doubtful => "D",
            Self::Out => "OUT",
            Self::IR => "IR",
            Self::Suspended => "SUSP",
            Self::Unknown => "?",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_display_round_trips_through_from_str() {
        // The AI is shown slots via Display and echoes them back through
        // from_str — every starter slot must survive the round trip.
        for pos in [
            Position::QB,
            Position::RB,
            Position::WR,
            Position::TE,
            Position::K,
            Position::DST,
            Position::FLEX,
            Position::SUPERFLEX,
        ] {
            assert_eq!(Position::from_str(&pos.to_string()), pos, "{pos} round trip");
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    pub id: String,
    pub name: String,
    pub position: Position,
    pub roster_slot: Position,
    pub team: String,
    pub projected_points: f32,
    pub avg_points: f32,
    pub status: PlayerStatus,
    pub opponent: Option<String>,
    pub bye_week: Option<u8>,
    pub news: Vec<String>,
}

impl Player {
    pub fn eligible_for(&self, slot: Position) -> bool {
        if slot == self.position {
            return true;
        }
        match slot {
            Position::FLEX => matches!(self.position, Position::RB | Position::WR | Position::TE),
            Position::SUPERFLEX => matches!(
                self.position,
                Position::QB | Position::RB | Position::WR | Position::TE
            ),
            Position::BENCH => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Roster {
    pub team_id: String,
    pub team_name: String,
    pub owner: Option<String>,
    pub players: Vec<Player>,
    pub wins: u32,
    pub losses: u32,
    pub ties: u32,
    pub points_for: f32,
    pub points_against: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Matchup {
    pub week: u8,
    pub home_team: String,
    pub away_team: String,
    pub home_projected: f32,
    pub away_projected: f32,
    pub home_score: f32,
    pub away_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPick {
    pub pick_number: u32,
    pub round: u32,
    pub team_id: String,
    pub team_name: String,
    pub player_id: Option<String>,
    pub player_name: Option<String>,
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftState {
    pub picks: Vec<DraftPick>,
    pub current_pick: u32,
    pub on_the_clock_team: Option<String>,
    /// Sleeper user_id of the drafter on the clock — use this (not the display
    /// name) for "is it my turn" checks; team names can be customized.
    pub on_the_clock_user_id: Option<String>,
    pub total_rounds: u32,
    pub team_count: u32,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineupSlot {
    pub slot: Position,
    pub player: Option<Player>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lineup {
    pub week: u8,
    pub starters: Vec<LineupSlot>,
    pub bench: Vec<Player>,
    pub projected_total: f32,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeagueSettings {
    pub scoring: String,
    pub roster_slots: Vec<(Position, u32)>,
    pub team_count: u32,
}

impl Default for LeagueSettings {
    fn default() -> Self {
        Self {
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
            team_count: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Sleeper-native types
// ---------------------------------------------------------------------------

/// A league transaction (waiver claim, free-agent add/drop, or trade).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub kind: String,          // "waiver" | "free_agent" | "trade" | "commissioner"
    pub status: String,        // "complete" | "failed" | ...
    pub week: u8,
    pub teams: Vec<String>,    // team names involved
    pub adds: Vec<(String, String)>,  // (player name, team name)
    pub drops: Vec<(String, String)>,
    pub waiver_bid: Option<u32>,      // FAAB dollars, if any
    pub traded_picks: Vec<String>,    // human-readable pick descriptions
}

/// A future draft pick that has changed hands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradedPick {
    pub season: String,
    pub round: u8,
    pub original_owner: String,
    pub current_owner: String,
}

/// One matchup in the winners/losers playoff bracket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BracketMatch {
    pub round: u8,
    pub match_id: u32,
    pub team1: Option<String>,
    pub team2: Option<String>,
    pub winner: Option<String>,
    pub placing: Option<u8>,   // e.g. Some(1) for the championship match
}

/// A player trending on Sleeper (most-added or most-dropped league-wide).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendingPlayer {
    pub player: Player,
    pub count: u64,            // adds or drops in the lookback window
    pub direction: TrendDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrendDirection {
    Add,
    Drop,
}

/// A league discovered for the configured user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredLeague {
    pub league_id: String,
    pub name: String,
    pub season: String,
    pub total_rosters: u32,
    pub scoring: String,
}
