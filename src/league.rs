//! League-wide comparison: how each roster stacks up position by position.
//!
//! The point is to answer "where is my team actually weak?", which raw totals
//! do not: a team can lead the league in points while carrying a hole at tight
//! end. So strength is measured per position, and separately for the players
//! who start every week and the depth behind them.

use crate::types::{Player, Position, Roster};

/// Positions worth ranking. FLEX and friends are omitted — they are slots, not
/// player positions, and a flex player is already counted at his real one.
pub const RANKED: &[Position] = &[
    Position::QB,
    Position::RB,
    Position::WR,
    Position::TE,
    Position::K,
    Position::DST,
];

/// One team's standing at one position.
#[derive(Debug, Clone)]
pub struct PosRank {
    pub position: Position,
    /// Projected points from players currently in a starting slot.
    pub starters: f32,
    /// Projected points from everyone else at this position.
    pub bench: f32,
    /// 1 = best in the league on starter value.
    pub rank_starters: u32,
    /// 1 = best in the league on bench value.
    pub rank_bench: u32,
    pub teams: u32,
    pub league_avg_starters: f32,
}

impl PosRank {
    /// Starter rank as 0..1 where 1 is the best team in the league. This is
    /// what the radar chart plots — rank rather than raw points, so positions
    /// that simply score more (QB) do not dominate the shape.
    pub fn starter_score(&self) -> f32 {
        if self.teams <= 1 {
            return 1.0;
        }
        (self.teams - self.rank_starters) as f32 / (self.teams - 1) as f32
    }

    /// Points above or below the league average at this position.
    pub fn vs_average(&self) -> f32 {
        self.starters - self.league_avg_starters
    }

    /// Bottom third of the league on starters — the positions worth upgrading.
    pub fn is_weakness(&self) -> bool {
        self.teams >= 3 && self.rank_starters * 3 > self.teams * 2
    }

    /// Top third of the league — surplus worth trading from.
    pub fn is_strength(&self) -> bool {
        self.teams >= 3 && self.rank_starters * 3 <= self.teams
    }
}

/// Starter and bench projected points for one roster at one position.
fn value_at(roster: &Roster, pos: Position) -> (f32, f32) {
    let (mut starters, mut bench) = (0.0, 0.0);
    for p in roster.players.iter().filter(|p| p.position == pos) {
        if p.roster_slot.is_starter_slot() {
            starters += p.projected_points;
        } else {
            bench += p.projected_points;
        }
    }
    (starters, bench)
}

/// Rank `team_name` against every roster in the league, position by position.
/// Returns an entry per ranked position, in `RANKED` order.
pub fn rank_team(all: &[Roster], team_name: &str) -> Vec<PosRank> {
    let teams = all.len() as u32;
    RANKED
        .iter()
        .map(|&pos| {
            let values: Vec<(String, f32, f32)> = all
                .iter()
                .map(|r| {
                    let (s, b) = value_at(r, pos);
                    (r.team_name.clone(), s, b)
                })
                .collect();

            let (starters, bench) = values
                .iter()
                .find(|(n, _, _)| n == team_name)
                .map(|(_, s, b)| (*s, *b))
                .unwrap_or((0.0, 0.0));

            // Rank is "how many teams are strictly better, plus one", so ties
            // share a rank rather than being ordered arbitrarily.
            let rank_starters = 1 + values.iter().filter(|(_, s, _)| *s > starters).count() as u32;
            let rank_bench = 1 + values.iter().filter(|(_, _, b)| *b > bench).count() as u32;
            let league_avg_starters = if teams == 0 {
                0.0
            } else {
                values.iter().map(|(_, s, _)| *s).sum::<f32>() / teams as f32
            };

            PosRank {
                position: pos,
                starters,
                bench,
                rank_starters,
                rank_bench,
                teams,
                league_avg_starters,
            }
        })
        .collect()
}

/// The starters a team fields at a position, best first — used to explain a
/// ranking rather than just asserting it.
pub fn starters_at(roster: &Roster, pos: Position) -> Vec<&Player> {
    let mut v: Vec<&Player> = roster
        .players
        .iter()
        .filter(|p| p.position == pos && p.roster_slot.is_starter_slot())
        .collect();
    v.sort_by(|a, b| {
        b.projected_points
            .partial_cmp(&a.projected_points)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PlayerStatus;

    fn player(name: &str, pos: Position, slot: Position, pts: f32) -> Player {
        Player {
            id: name.to_string(),
            name: name.to_string(),
            position: pos,
            roster_slot: slot,
            team: "NE".into(),
            projected_points: pts,
            avg_points: 0.0,
            status: PlayerStatus::Healthy,
            opponent: None,
            bye_week: None,
            news: vec![],
        }
    }

    fn roster(name: &str, players: Vec<Player>) -> Roster {
        Roster {
            team_id: name.into(),
            team_name: name.into(),
            owner: None,
            players,
            wins: 0,
            losses: 0,
            ties: 0,
            points_for: 0.0,
            points_against: 0.0,
        }
    }

    fn league() -> Vec<Roster> {
        vec![
            roster(
                "Mine",
                vec![
                    player("qb1", Position::QB, Position::QB, 20.0),
                    player("rb1", Position::RB, Position::RB, 5.0),
                    player("rb2", Position::RB, Position::BENCH, 9.0),
                ],
            ),
            roster(
                "Rival",
                vec![
                    player("qb2", Position::QB, Position::QB, 10.0),
                    player("rb3", Position::RB, Position::RB, 15.0),
                    player("rb4", Position::RB, Position::BENCH, 1.0),
                ],
            ),
            roster(
                "Third",
                vec![
                    player("qb3", Position::QB, Position::QB, 15.0),
                    player("rb5", Position::RB, Position::RB, 10.0),
                ],
            ),
        ]
    }

    #[test]
    fn ranks_by_starter_value_within_position() {
        let ranks = rank_team(&league(), "Mine");
        let qb = ranks.iter().find(|r| r.position == Position::QB).unwrap();
        assert_eq!(qb.starters, 20.0);
        assert_eq!(qb.rank_starters, 1, "20 is the best QB total");

        let rb = ranks.iter().find(|r| r.position == Position::RB).unwrap();
        assert_eq!(rb.starters, 5.0);
        assert_eq!(rb.rank_starters, 3, "5 is the worst RB starter total");
    }

    #[test]
    fn bench_is_ranked_separately_from_starters() {
        let ranks = rank_team(&league(), "Mine");
        let rb = ranks.iter().find(|r| r.position == Position::RB).unwrap();
        // Worst starters, best bench — the two must not be conflated.
        assert_eq!(rb.rank_starters, 3);
        assert_eq!(rb.bench, 9.0);
        assert_eq!(rb.rank_bench, 1);
    }

    #[test]
    fn strength_and_weakness_split_the_league_into_thirds() {
        let ranks = rank_team(&league(), "Mine");
        let qb = ranks.iter().find(|r| r.position == Position::QB).unwrap();
        let rb = ranks.iter().find(|r| r.position == Position::RB).unwrap();
        assert!(qb.is_strength() && !qb.is_weakness());
        assert!(rb.is_weakness() && !rb.is_strength());
    }

    #[test]
    fn starter_score_spans_zero_to_one_and_feeds_the_radar() {
        let ranks = rank_team(&league(), "Mine");
        let qb = ranks.iter().find(|r| r.position == Position::QB).unwrap();
        let rb = ranks.iter().find(|r| r.position == Position::RB).unwrap();
        assert_eq!(qb.starter_score(), 1.0, "rank 1 of 3 plots at the outer edge");
        assert_eq!(rb.starter_score(), 0.0, "last of 3 plots at the centre");
    }

    #[test]
    fn ties_share_a_rank_rather_than_being_ordered_arbitrarily() {
        let l = vec![
            roster("A", vec![player("a", Position::QB, Position::QB, 10.0)]),
            roster("B", vec![player("b", Position::QB, Position::QB, 10.0)]),
        ];
        for team in ["A", "B"] {
            let r = rank_team(&l, team);
            let qb = r.iter().find(|r| r.position == Position::QB).unwrap();
            assert_eq!(qb.rank_starters, 1, "{team} should share first place");
        }
    }

    #[test]
    fn a_position_nobody_rosters_does_not_panic() {
        let ranks = rank_team(&league(), "Mine");
        let k = ranks.iter().find(|r| r.position == Position::K).unwrap();
        assert_eq!(k.starters, 0.0);
        assert_eq!(k.rank_starters, 1, "all tied at zero");
        assert_eq!(k.vs_average(), 0.0);
    }

    #[test]
    fn unknown_team_name_yields_zeroes_not_a_panic() {
        let ranks = rank_team(&league(), "Nobody");
        assert!(ranks.iter().all(|r| r.starters == 0.0));
    }
}
