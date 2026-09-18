//! Per-player detail: upcoming schedule, season production, and how reliably
//! a player beats his own projection.
//!
//! The accuracy record is the expensive part. It needs both the projection and
//! the actual for every completed week, which is two API calls per week — but
//! each call returns *every* player, so one pass builds the table for the whole
//! league at once. Completed weeks never change, so the result is cached on
//! disk and only rebuilt when another week finishes.

use crate::api::{ApiScheduleGame, SleeperClient};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A scheduled game for one team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpcomingGame {
    pub week: u8,
    pub opponent: String,
    pub home: bool,
    pub date: Option<String>,
}

impl UpcomingGame {
    /// "vs BUF" / "@ BUF" — the conventional shorthand.
    pub fn label(&self) -> String {
        if self.home {
            format!("vs {}", self.opponent)
        } else {
            format!("@ {}", self.opponent)
        }
    }
}

/// How a player performed against his weekly projection over a season.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfRecord {
    pub games: u32,
    /// Games where the actual score exceeded the projection.
    pub beat: u32,
    pub total_proj: f32,
    pub total_actual: f32,
    /// Best and worst single-week differential, for a sense of the spread.
    pub best_diff: f32,
    pub worst_diff: f32,
    /// Season-to-date raw stat totals (yards, TDs, receptions …), accumulated
    /// in the same pass so the detail view needs no extra requests.
    #[serde(default)]
    pub totals: HashMap<String, f64>,
}

impl PerfRecord {
    /// Share of games the player outperformed his projection, 0-100.
    pub fn beat_pct(&self) -> f32 {
        if self.games == 0 {
            0.0
        } else {
            100.0 * self.beat as f32 / self.games as f32
        }
    }

    pub fn avg_proj(&self) -> f32 {
        if self.games == 0 {
            0.0
        } else {
            self.total_proj / self.games as f32
        }
    }

    pub fn avg_actual(&self) -> f32 {
        if self.games == 0 {
            0.0
        } else {
            self.total_actual / self.games as f32
        }
    }

    /// Mean points above (positive) or below (negative) projection per game.
    pub fn avg_diff(&self) -> f32 {
        self.avg_actual() - self.avg_proj()
    }
}

/// Projection-accuracy records for every player, over one season.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfTable {
    /// Season the records were computed from. May lag the current season when
    /// the new one has not kicked off yet — surfaced in the UI so a stat from
    /// last year is never mistaken for this year's.
    pub season: String,
    /// Completed weeks the table covers.
    pub weeks: u32,
    pub records: HashMap<String, PerfRecord>,
}

impl PerfTable {
    pub fn get(&self, player_id: &str) -> Option<&PerfRecord> {
        self.records.get(player_id)
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

fn cache_path(season: &str, weeks: u32) -> PathBuf {
    let dir = match std::env::var("SA_CACHE_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sleeper-agent"),
    };
    // Week count is in the filename, so finishing another week naturally
    // misses the cache instead of serving a stale table.
    dir.join(format!("perf-{season}-w{weeks}.json"))
}

/// Weeks with at least one completed game, and the season they belong to.
/// Falls back to the previous season when the current one has not started —
/// otherwise every record would be empty through the whole preseason.
pub async fn scoring_season(
    client: &SleeperClient,
    current_season: &str,
    previous_season: &str,
) -> Result<(String, Vec<u8>)> {
    let completed = |games: &[ApiScheduleGame]| -> Vec<u8> {
        let mut w: Vec<u8> = games
            .iter()
            .filter(|g| g.is_complete())
            .map(|g| g.week)
            .collect();
        w.sort_unstable();
        w.dedup();
        w
    };

    let current = client.schedule(current_season).await?;
    let weeks = completed(&current);
    if !weeks.is_empty() {
        return Ok((current_season.to_string(), weeks));
    }
    let prev = client.schedule(previous_season).await?;
    Ok((previous_season.to_string(), completed(&prev)))
}

/// Build the accuracy table, reading from disk when a matching one is cached.
pub async fn build_perf_table(
    client: &SleeperClient,
    season: &str,
    weeks: &[u8],
    scoring_key: &str,
) -> Result<PerfTable> {
    if weeks.is_empty() {
        return Ok(PerfTable {
            season: season.to_string(),
            ..Default::default()
        });
    }

    let path = cache_path(season, weeks.len() as u32);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(t) = serde_json::from_slice::<PerfTable>(&bytes) {
            tracing::debug!(season, weeks = weeks.len(), "perf table from disk cache");
            return Ok(t);
        }
    }

    tracing::info!(
        season,
        weeks = weeks.len(),
        "building projection-accuracy table (2 requests per week, cached afterwards)"
    );

    let mut records: HashMap<String, PerfRecord> = HashMap::new();
    for &w in weeks {
        // A single failed week degrades the table rather than sinking it;
        // partial history still beats none.
        let (proj, actual) = match tokio::try_join!(
            client.projections(season, w),
            client.stats(season, w)
        ) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(week = w, error = %e, "skipping week in accuracy table");
                continue;
            }
        };

        for (pid, astats) in &actual {
            // Only weeks the player actually played: a projection with no
            // snap behind it says nothing about whether he beats it.
            if astats.get("gp").copied().unwrap_or(0.0) < 1.0 {
                continue;
            }
            let Some(pstats) = proj.get(pid) else { continue };
            let p = pstats
                .get(scoring_key)
                .or_else(|| pstats.get("pts_ppr"))
                .copied()
                .unwrap_or(0.0) as f32;
            let a = astats
                .get(scoring_key)
                .or_else(|| astats.get("pts_ppr"))
                .copied()
                .unwrap_or(0.0) as f32;
            // No projection means no comparison to make.
            if p <= 0.0 {
                continue;
            }
            let r = records.entry(pid.clone()).or_default();
            r.games += 1;
            if a > p {
                r.beat += 1;
            }
            r.total_proj += p;
            r.total_actual += a;
            for (k, v) in astats {
                // Counting stats only — rankings and per-game averages do not
                // sum meaningfully across weeks.
                if k.starts_with("pts_") || k.contains("rank") || k.contains("adp") {
                    continue;
                }
                *r.totals.entry(k.clone()).or_insert(0.0) += v;
            }
            let d = a - p;
            if r.games == 1 {
                r.best_diff = d;
                r.worst_diff = d;
            } else {
                r.best_diff = r.best_diff.max(d);
                r.worst_diff = r.worst_diff.min(d);
            }
        }
    }

    let table = PerfTable {
        season: season.to_string(),
        weeks: weeks.len() as u32,
        records,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&table) {
        let _ = std::fs::write(&path, bytes);
    }
    Ok(table)
}

/// Next `count` games for a team, starting at `from_week`.
pub fn upcoming_for_team(
    schedule: &[ApiScheduleGame],
    team: &str,
    from_week: u8,
    count: usize,
) -> Vec<UpcomingGame> {
    if team.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<UpcomingGame> = schedule
        .iter()
        .filter(|g| g.week >= from_week)
        .filter_map(|g| {
            let home = g.home.as_deref().unwrap_or_default();
            let away = g.away.as_deref().unwrap_or_default();
            if home.eq_ignore_ascii_case(team) {
                Some(UpcomingGame {
                    week: g.week,
                    opponent: away.to_string(),
                    home: true,
                    date: g.date.clone(),
                })
            } else if away.eq_ignore_ascii_case(team) {
                Some(UpcomingGame {
                    week: g.week,
                    opponent: home.to_string(),
                    home: false,
                    date: g.date.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    out.sort_by_key(|g| g.week);
    out.truncate(count);
    out
}

/// Season-to-date totals worth showing, in a stable display order. Returns the
/// subset of `stats` that is both present and non-zero for this position.
pub fn notable_stats(stats: &HashMap<String, f64>, position: &str) -> Vec<(String, f64)> {
    const COMMON: &[(&str, &str)] = &[("gp", "Games")];
    let by_pos: &[(&str, &str)] = match position {
        "QB" => &[
            ("pass_yd", "Pass yds"),
            ("pass_td", "Pass TD"),
            ("pass_int", "INT"),
            ("rush_yd", "Rush yds"),
            ("rush_td", "Rush TD"),
        ],
        "RB" => &[
            ("rush_att", "Carries"),
            ("rush_yd", "Rush yds"),
            ("rush_td", "Rush TD"),
            ("rec", "Rec"),
            ("rec_yd", "Rec yds"),
            ("rec_td", "Rec TD"),
        ],
        "WR" | "TE" => &[
            ("rec_tgt", "Targets"),
            ("rec", "Rec"),
            ("rec_yd", "Rec yds"),
            ("rec_td", "Rec TD"),
        ],
        "K" => &[("fgm", "FG made"), ("fga", "FG att"), ("xpm", "XP made")],
        "DST" => &[
            ("def_sack", "Sacks"),
            ("def_int", "INT"),
            ("def_fum_rec", "Fum rec"),
            ("def_td", "TD"),
        ],
        _ => &[],
    };
    COMMON
        .iter()
        .chain(by_pos.iter())
        .filter_map(|(key, label)| {
            let v = stats.get(*key).copied()?;
            (v != 0.0).then(|| ((*label).to_string(), v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_pct_and_diff_are_computed_per_game() {
        let r = PerfRecord {
            games: 4,
            beat: 3,
            total_proj: 40.0,
            total_actual: 50.0,
            best_diff: 8.0,
            worst_diff: -2.0,
            ..Default::default()
        };
        assert_eq!(r.beat_pct(), 75.0);
        assert_eq!(r.avg_proj(), 10.0);
        assert_eq!(r.avg_actual(), 12.5);
        assert_eq!(r.avg_diff(), 2.5);
    }

    #[test]
    fn empty_record_does_not_divide_by_zero() {
        let r = PerfRecord::default();
        assert_eq!(r.beat_pct(), 0.0);
        assert_eq!(r.avg_diff(), 0.0);
    }

    fn game(week: u8, home: &str, away: &str) -> ApiScheduleGame {
        ApiScheduleGame {
            status: "pre_game".into(),
            date: Some("2026-09-13".into()),
            home: Some(home.into()),
            away: Some(away.into()),
            week,
            game_id: None,
        }
    }

    #[test]
    fn upcoming_picks_the_right_side_and_respects_the_start_week() {
        let sched = vec![
            game(1, "CAR", "CHI"),
            game(2, "CHI", "DET"),
            game(3, "GB", "CHI"),
        ];
        let g = upcoming_for_team(&sched, "CHI", 2, 5);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].label(), "vs DET");
        assert_eq!(g[1].label(), "@ GB");
    }

    #[test]
    fn upcoming_is_empty_for_a_player_with_no_team() {
        assert!(upcoming_for_team(&[game(1, "CAR", "CHI")], "", 1, 5).is_empty());
    }

    #[test]
    fn notable_stats_skips_absent_and_zero_lines() {
        let stats = HashMap::from([
            ("gp".to_string(), 3.0),
            ("rec".to_string(), 12.0),
            ("rec_td".to_string(), 0.0),
        ]);
        let out = notable_stats(&stats, "WR");
        let labels: Vec<&str> = out.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["Games", "Rec"]);
    }
}
