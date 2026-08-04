//! Predictive metrics for an individual player or trade package.
//!
//! All values are derived deterministically from the data we already have
//! (projection, recent average, injury status, position, league context).
//! The numbers are intentionally simple — the AI layer reads them as a
//! signal, not as ground truth.

use crate::strategy::Strategy;
use crate::types::*;

#[derive(Debug, Clone)]
pub struct PlayerMetrics {
    pub player_id: String,
    pub player_name: String,
    pub position: Position,
    pub team: String,
    pub mean_projection: f32,
    pub floor: f32,
    pub ceiling: f32,
    pub variance: f32,
    /// 0.0 = healthy/no risk, 1.0 = max risk (Out, IR, suspended).
    pub risk_score: f32,
    /// Negative = trending down vs avg, Positive = trending up.
    pub trend: f32,
    /// Multiplier applied for injury status under the chosen strategy.
    pub injury_adj: f32,
    /// Risk-adjusted points for the next single week.
    pub adjusted_next_week: f32,
    /// Rough rest-of-season expected points (assume 14 weeks remaining).
    pub ros_value: f32,
    /// 0.0–1.0 — how well the player matches the chosen strategy.
    pub strategy_fit: f32,
}

impl PlayerMetrics {
    pub fn for_player(p: &Player, strat: Strategy) -> Self {
        let mean = if p.projected_points > 0.0 {
            p.projected_points
        } else {
            p.avg_points
        };
        let injury_adj = match p.status {
            PlayerStatus::Healthy => 1.0,
            PlayerStatus::Questionable => strat.questionable_multiplier(),
            PlayerStatus::Doubtful => strat.doubtful_multiplier(),
            PlayerStatus::Out | PlayerStatus::IR | PlayerStatus::Suspended => 0.0,
            PlayerStatus::Unknown => 0.95,
        };
        let risk_score = match p.status {
            PlayerStatus::Healthy => 0.05,
            PlayerStatus::Questionable => 0.30,
            PlayerStatus::Doubtful => 0.60,
            PlayerStatus::Out | PlayerStatus::Suspended => 0.95,
            PlayerStatus::IR => 1.0,
            PlayerStatus::Unknown => 0.10,
        };
        // Variance estimate scales with position volatility.
        let position_var = match p.position {
            Position::QB => 0.18,
            Position::RB => 0.32,
            Position::WR => 0.38,
            Position::TE => 0.42,
            Position::K => 0.55,
            Position::DST => 0.50,
            _ => 0.30,
        };
        let variance = (mean.max(1.0)) * position_var;
        let floor = (mean - variance).max(0.0);
        let ceiling = mean + variance * 1.4;

        // Trend uses (avg - projection) as a noisy signal of overperformance.
        // avg_points isn't populated yet (always 0), so without the guard this
        // would read as a constant -1.0 "trending down" for every player.
        let trend = if mean > 0.0 && p.avg_points > 0.0 {
            ((p.avg_points - p.projected_points) / mean).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        let adjusted_next_week = mean * injury_adj;
        let ros_value = adjusted_next_week * 14.0;

        let strategy_fit = match strat {
            Strategy::Conservative => {
                // reward low variance + healthy status
                (1.0 - position_var) * (1.0 - risk_score)
            }
            Strategy::Balanced => {
                // reward expected value with mild variance penalty
                let ev_norm = (adjusted_next_week / 25.0).clamp(0.0, 1.0);
                ev_norm * (1.0 - 0.4 * risk_score)
            }
            Strategy::HighStakes => {
                // reward ceiling more than mean
                let ceil_norm = (ceiling / 35.0).clamp(0.0, 1.0);
                ceil_norm * (1.0 - 0.2 * risk_score) + 0.1 * trend
            }
        }
        .clamp(0.0, 1.0);

        Self {
            player_id: p.id.clone(),
            player_name: p.name.clone(),
            position: p.position,
            team: p.team.clone(),
            mean_projection: mean,
            floor,
            ceiling,
            variance,
            risk_score,
            trend,
            injury_adj,
            adjusted_next_week,
            ros_value,
            strategy_fit,
        }
    }

    pub fn one_line(&self) -> String {
        format!(
            "{} {} {}: mean {:.1} (floor {:.1}, ceil {:.1}) ROS {:.0} risk {:.2} fit {:.2}",
            self.player_name,
            self.position,
            self.team,
            self.mean_projection,
            self.floor,
            self.ceiling,
            self.ros_value,
            self.risk_score,
            self.strategy_fit
        )
    }
}

#[derive(Debug, Clone)]
pub struct PackageMetrics {
    pub total_mean: f32,
    pub total_floor: f32,
    pub total_ceiling: f32,
    pub total_ros: f32,
    pub avg_risk: f32,
    pub avg_fit: f32,
    pub by_position: Vec<(Position, u32)>,
}

impl PackageMetrics {
    pub fn from(metrics: &[PlayerMetrics]) -> Self {
        if metrics.is_empty() {
            return Self {
                total_mean: 0.0,
                total_floor: 0.0,
                total_ceiling: 0.0,
                total_ros: 0.0,
                avg_risk: 0.0,
                avg_fit: 0.0,
                by_position: vec![],
            };
        }
        let n = metrics.len() as f32;
        let mut counts = std::collections::HashMap::new();
        for m in metrics {
            *counts.entry(m.position).or_insert(0u32) += 1;
        }
        let mut by_position: Vec<_> = counts.into_iter().collect();
        by_position.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        Self {
            total_mean: metrics.iter().map(|m| m.mean_projection).sum(),
            total_floor: metrics.iter().map(|m| m.floor).sum(),
            total_ceiling: metrics.iter().map(|m| m.ceiling).sum(),
            total_ros: metrics.iter().map(|m| m.ros_value).sum(),
            avg_risk: metrics.iter().map(|m| m.risk_score).sum::<f32>() / n,
            avg_fit: metrics.iter().map(|m| m.strategy_fit).sum::<f32>() / n,
            by_position,
        }
    }
}
