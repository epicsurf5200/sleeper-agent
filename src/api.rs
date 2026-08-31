//! Full-coverage Sleeper API client.
//!
//! Endpoints used (https://docs.sleeper.com/):
//!   GET /v1/state/nfl                                  — current week/season
//!   GET /v1/user/{username|user_id}                    — user lookup
//!   GET /v1/user/{user_id}/leagues/nfl/{season}        — league discovery
//!   GET /v1/league/{id}                                — league settings
//!   GET /v1/league/{id}/rosters                        — all rosters
//!   GET /v1/league/{id}/users                          — league members
//!   GET /v1/league/{id}/matchups/{week}                — weekly matchups
//!   GET /v1/league/{id}/winners_bracket                — playoff bracket
//!   GET /v1/league/{id}/losers_bracket                 — consolation bracket
//!   GET /v1/league/{id}/transactions/{week}            — waivers/trades/FA
//!   GET /v1/league/{id}/traded_picks                   — future picks moved
//!   GET /v1/league/{id}/drafts                         — league drafts
//!   GET /v1/draft/{id}  + /picks + /traded_picks       — draft detail
//!   GET /v1/players/nfl                                — full player DB (disk-cached 24h)
//!   GET /v1/players/nfl/trending/{add|drop}            — trending players
//!   GET /v1/projections/nfl/regular/{season}/{week}    — weekly projections
//!   GET /v1/stats/nfl/regular/{season}/{week}          — weekly actuals
//!
//! The player DB is ~5 MB; Sleeper asks that it be fetched at most once per
//! day, so it is cached on disk under the user cache directory.

use crate::types::*;
use anyhow::{anyhow, Context, Result};
use parking_lot::RwLock;
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const BASE: &str = "https://api.sleeper.app/v1";
const PLAYER_CACHE_TTL_SECS: u64 = 24 * 3600;

// ---------------------------------------------------------------------------
// Raw API shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ApiScheduleGame {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub home: Option<String>,
    #[serde(default)]
    pub away: Option<String>,
    #[serde(default)]
    pub week: u8,
    #[serde(default)]
    pub game_id: Option<String>,
}

impl ApiScheduleGame {
    /// A game with a final score behind it. Used to decide which weeks can
    /// contribute to a projection-accuracy record.
    pub fn is_complete(&self) -> bool {
        self.status.eq_ignore_ascii_case("complete")
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiState {
    pub week: u8,
    pub season: String,
    pub season_type: String,
    #[serde(default)]
    pub display_week: Option<u8>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiUser {
    pub user_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiLeague {
    pub league_id: String,
    pub name: String,
    pub season: String,
    pub total_rosters: u32,
    #[serde(default)]
    pub status: String,
    pub roster_positions: Vec<String>,
    pub scoring_settings: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub previous_league_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiRoster {
    pub roster_id: u32,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub players: Option<Vec<String>>,
    #[serde(default)]
    pub starters: Option<Vec<String>>,
    #[serde(default)]
    pub reserve: Option<Vec<String>>,
    #[serde(default)]
    pub taxi: Option<Vec<String>>,
    #[serde(default)]
    pub settings: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiPlayer {
    #[serde(default)]
    pub player_id: String,
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub position: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub injury_status: Option<String>,
    #[serde(default)]
    pub injury_notes: Option<String>,
    #[serde(default)]
    pub age: Option<u32>,
    #[serde(default)]
    pub years_exp: Option<u32>,
    #[serde(default)]
    pub number: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiMatchup {
    pub matchup_id: Option<u32>,
    pub roster_id: u32,
    #[serde(default)]
    pub points: f32,
    #[serde(default)]
    pub starters: Option<Vec<String>>,
    #[serde(default)]
    pub players_points: Option<HashMap<String, f32>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiBracketMatch {
    pub r: u8,
    pub m: u32,
    #[serde(default)]
    pub t1: Option<serde_json::Value>,
    #[serde(default)]
    pub t2: Option<serde_json::Value>,
    #[serde(default)]
    pub w: Option<u32>,
    #[serde(default)]
    pub p: Option<u8>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiTransaction {
    pub transaction_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub roster_ids: Option<Vec<u32>>,
    #[serde(default)]
    pub adds: Option<HashMap<String, u32>>,
    #[serde(default)]
    pub drops: Option<HashMap<String, u32>>,
    #[serde(default)]
    pub settings: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    pub draft_picks: Option<Vec<ApiTradedPick>>,
    #[serde(default)]
    pub leg: Option<u8>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiTradedPick {
    pub season: String,
    pub round: u8,
    #[serde(default)]
    pub roster_id: Option<u32>,
    #[serde(default)]
    pub owner_id: Option<u32>,
    #[serde(default)]
    pub previous_owner_id: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiDraft {
    pub draft_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub draft_order: Option<HashMap<String, u32>>,
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiDraftPick {
    pub pick_no: u32,
    pub round: u32,
    #[serde(default)]
    pub roster_id: Option<u32>,
    #[serde(default)]
    pub picked_by: Option<String>,
    pub player_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiTrending {
    pub player_id: String,
    pub count: u64,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Full player DB: player_id → ApiPlayer.
pub type PlayerDb = Arc<HashMap<String, ApiPlayer>>;
/// Weekly projections: player_id → stat map (pts_ppr / pts_half_ppr / pts_std …).
pub type ProjectionMap = Arc<HashMap<String, HashMap<String, f64>>>;

pub struct SleeperClient {
    http: Client,
    /// API base — `https://api.sleeper.app/v1`, overridable via
    /// `SLEEPER_API_BASE` for tests/simulations against a mock server.
    base: String,
    /// Cached full player DB.
    players: Arc<RwLock<Option<PlayerDb>>>,
    /// Serializes the 5 MB player-DB download (single-flight).
    players_fetch: Arc<tokio::sync::Mutex<()>>,
    /// Cached weekly projections keyed by (season, week).
    projections: Arc<RwLock<HashMap<(String, u8), ProjectionMap>>>,
    /// Cached NFL schedule keyed by season. One 27 KB document covers the
    /// whole year, so this is fetched once and reused.
    schedule: Arc<RwLock<HashMap<String, Arc<Vec<ApiScheduleGame>>>>>,
    cache_dir: PathBuf,
}

/// Best-effort read of the on-disk player DB cache.
fn read_player_cache(path: &Path) -> Option<HashMap<String, ApiPlayer>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl SleeperClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("sleeper-agent/0.1 (+https://github.com/epicsurf5200/sleeper-agent)")
            .build()?;
        let cache_dir = match std::env::var("SA_CACHE_DIR") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("sleeper-agent"),
        };
        let base = std::env::var("SLEEPER_API_BASE").unwrap_or_else(|_| BASE.to_string());
        Ok(Self {
            http,
            base,
            players: Arc::new(RwLock::new(None)),
            players_fetch: Arc::new(tokio::sync::Mutex::new(())),
            projections: Arc::new(RwLock::new(HashMap::new())),
            schedule: Arc::new(RwLock::new(HashMap::new())),
            cache_dir,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        // URLs are formatted against the BASE const; swap in the override here
        // (single point) rather than threading the base through every call site.
        let url = if self.base == BASE {
            url.to_string()
        } else {
            url.replacen(BASE, &self.base, 1)
        };
        let url = url.as_str();
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            return Err(anyhow!("sleeper API {status} on {url}: {snippet}"));
        }
        resp.json::<T>().await.with_context(|| format!("decoding {url}"))
    }

    // -- state / users / leagues ---------------------------------------------

    pub async fn state(&self) -> Result<ApiState> {
        self.get_json(&format!("{BASE}/state/nfl")).await
    }

    pub async fn user(&self, username_or_id: &str) -> Result<ApiUser> {
        self.get_json(&format!("{BASE}/user/{username_or_id}")).await
    }

    pub async fn user_leagues(&self, user_id: &str, season: &str) -> Result<Vec<ApiLeague>> {
        self.get_json(&format!("{BASE}/user/{user_id}/leagues/nfl/{season}"))
            .await
    }

    pub async fn league(&self, league_id: &str) -> Result<ApiLeague> {
        self.get_json(&format!("{BASE}/league/{league_id}")).await
    }

    pub async fn league_rosters(&self, league_id: &str) -> Result<Vec<ApiRoster>> {
        self.get_json(&format!("{BASE}/league/{league_id}/rosters")).await
    }

    pub async fn league_users(&self, league_id: &str) -> Result<Vec<ApiUser>> {
        self.get_json(&format!("{BASE}/league/{league_id}/users")).await
    }

    pub async fn league_matchups(&self, league_id: &str, week: u8) -> Result<Vec<ApiMatchup>> {
        self.get_json(&format!("{BASE}/league/{league_id}/matchups/{week}"))
            .await
    }

    pub async fn winners_bracket(&self, league_id: &str) -> Result<Vec<ApiBracketMatch>> {
        self.get_json(&format!("{BASE}/league/{league_id}/winners_bracket"))
            .await
    }

    pub async fn losers_bracket(&self, league_id: &str) -> Result<Vec<ApiBracketMatch>> {
        self.get_json(&format!("{BASE}/league/{league_id}/losers_bracket"))
            .await
    }

    pub async fn transactions(&self, league_id: &str, week: u8) -> Result<Vec<ApiTransaction>> {
        self.get_json(&format!("{BASE}/league/{league_id}/transactions/{week}"))
            .await
    }

    pub async fn traded_picks(&self, league_id: &str) -> Result<Vec<ApiTradedPick>> {
        self.get_json(&format!("{BASE}/league/{league_id}/traded_picks"))
            .await
    }

    pub async fn league_drafts(&self, league_id: &str) -> Result<Vec<ApiDraft>> {
        self.get_json(&format!("{BASE}/league/{league_id}/drafts")).await
    }

    pub async fn draft(&self, draft_id: &str) -> Result<ApiDraft> {
        self.get_json(&format!("{BASE}/draft/{draft_id}")).await
    }

    pub async fn draft_picks(&self, draft_id: &str) -> Result<Vec<ApiDraftPick>> {
        self.get_json(&format!("{BASE}/draft/{draft_id}/picks")).await
    }

    // -- players (disk-cached), trending, projections ------------------------

    pub async fn all_players(&self) -> Result<Arc<HashMap<String, ApiPlayer>>> {
        if let Some(p) = self.players.read().clone() {
            return Ok(p);
        }
        // Single-flight: without this, N concurrent cache misses would each
        // download the 5 MB DB (Sleeper asks for at most one fetch per day).
        let _guard = self.players_fetch.lock().await;
        if let Some(p) = self.players.read().clone() {
            return Ok(p); // another caller fetched while we waited
        }

        let cache_file = self.cache_dir.join("players_nfl.json");
        let fresh = std::fs::metadata(&cache_file)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|age| age.as_secs() < PLAYER_CACHE_TTL_SECS)
            .unwrap_or(false);
        if fresh {
            if let Some(map) = read_player_cache(&cache_file) {
                let arc = Arc::new(map);
                *self.players.write() = Some(arc.clone());
                tracing::debug!("player DB loaded from disk cache");
                return Ok(arc);
            }
        }

        tracing::info!("fetching full player DB from Sleeper (~5 MB, cached 24h)");
        let map: HashMap<String, ApiPlayer> =
            match self.get_json(&format!("{BASE}/players/nfl")).await {
                Ok(map) => {
                    let _ = std::fs::create_dir_all(&self.cache_dir);
                    if let Ok(bytes) = serde_json::to_vec(&SerializablePlayers(&map)) {
                        let _ = std::fs::write(&cache_file, bytes);
                    }
                    map
                }
                Err(e) => {
                    // Network down / API error: a stale cache beats no data.
                    match read_player_cache(&cache_file) {
                        Some(map) => {
                            tracing::warn!(error=%e, "player DB fetch failed; using stale disk cache");
                            map
                        }
                        None => return Err(e),
                    }
                }
            };
        let arc = Arc::new(map);
        *self.players.write() = Some(arc.clone());
        Ok(arc)
    }

    pub async fn trending(&self, direction: TrendDirection, lookback_hours: u32, limit: u32) -> Result<Vec<ApiTrending>> {
        let dir = match direction {
            TrendDirection::Add => "add",
            TrendDirection::Drop => "drop",
        };
        self.get_json(&format!(
            "{BASE}/players/nfl/trending/{dir}?lookback_hours={lookback_hours}&limit={limit}"
        ))
        .await
    }

    /// Weekly projections: player_id → stat map (contains pts_ppr / pts_half_ppr / pts_std).
    pub async fn projections(&self, season: &str, week: u8) -> Result<Arc<HashMap<String, HashMap<String, f64>>>> {
        let key = (season.to_string(), week);
        if let Some(p) = self.projections.read().get(&key).cloned() {
            return Ok(p);
        }
        let raw: HashMap<String, Option<HashMap<String, f64>>> = self
            .get_json(&format!("{BASE}/projections/nfl/regular/{season}/{week}"))
            .await?;
        let map: HashMap<String, HashMap<String, f64>> = raw
            .into_iter()
            .filter_map(|(k, v)| v.map(|inner| (k, inner)))
            .collect();
        let arc = Arc::new(map);
        self.projections.write().insert(key, arc.clone());
        Ok(arc)
    }

    /// Full-season NFL schedule. Lives outside the /v1 namespace, hence the
    /// hand-built URL. Gives home/away and kickoff date, which the projections
    /// feed does not.
    pub async fn schedule(&self, season: &str) -> Result<Arc<Vec<ApiScheduleGame>>> {
        if let Some(s) = self.schedule.read().get(season).cloned() {
            return Ok(s);
        }
        let root = self.base.trim_end_matches("/v1");
        let games: Vec<ApiScheduleGame> = self
            .get_json(&format!("{root}/schedule/nfl/regular/{season}"))
            .await
            .with_context(|| format!("fetching {season} NFL schedule"))?;
        let arc = Arc::new(games);
        self.schedule.write().insert(season.to_string(), arc.clone());
        Ok(arc)
    }

    /// Weekly actual stats — same shape as projections.
    pub async fn stats(&self, season: &str, week: u8) -> Result<HashMap<String, HashMap<String, f64>>> {
        let raw: HashMap<String, Option<HashMap<String, f64>>> = self
            .get_json(&format!("{BASE}/stats/nfl/regular/{season}/{week}"))
            .await?;
        Ok(raw
            .into_iter()
            .filter_map(|(k, v)| v.map(|inner| (k, inner)))
            .collect())
    }
}

/// Wrapper so we can serialize the borrowed player map to disk.
struct SerializablePlayers<'a>(&'a HashMap<String, ApiPlayer>);

impl serde::Serialize for SerializablePlayers<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl serde::Serialize for ApiPlayer {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ApiPlayer", 11)?;
        st.serialize_field("player_id", &self.player_id)?;
        st.serialize_field("full_name", &self.full_name)?;
        st.serialize_field("first_name", &self.first_name)?;
        st.serialize_field("last_name", &self.last_name)?;
        st.serialize_field("position", &self.position)?;
        st.serialize_field("team", &self.team)?;
        st.serialize_field("injury_status", &self.injury_status)?;
        st.serialize_field("injury_notes", &self.injury_notes)?;
        st.serialize_field("age", &self.age)?;
        st.serialize_field("years_exp", &self.years_exp)?;
        st.serialize_field("number", &self.number)?;
        st.end()
    }
}

// ---------------------------------------------------------------------------
// High-level "league session" — resolves everything into domain types
// ---------------------------------------------------------------------------

pub struct LeagueSession {
    pub client: Arc<SleeperClient>,
    pub league_id: String,
    pub my_user_id: String,
    pub season: String,
    /// Scoring key into projection stat maps: pts_ppr / pts_half_ppr / pts_std.
    pub scoring_key: String,
    /// Short-TTL cache of (rosters, team names, users) — many methods need
    /// these, and one refresh cycle would otherwise hit the endpoints ~6×.
    teams_cache: RwLock<Option<(std::time::Instant, Arc<TeamsSnapshot>)>>,
}

type TeamsSnapshot = (Vec<ApiRoster>, HashMap<u32, String>, Vec<ApiUser>);

/// How long the rosters/users snapshot stays fresh.
const TEAMS_CACHE_TTL: Duration = Duration::from_secs(60);

impl LeagueSession {
    /// Resolve username → user_id, auto-discover the league when none is
    /// configured, and detect the league's scoring type.
    pub async fn connect(
        client: Arc<SleeperClient>,
        username_or_id: &str,
        league_id: Option<&str>,
    ) -> Result<Self> {
        let state = client.state().await?;
        let user = client.user(username_or_id).await?;
        let leagues = client.user_leagues(&user.user_id, &state.season).await?;
        let league = match league_id {
            // NB: `.or(client.league(id).await?)` would evaluate (and fetch)
            // eagerly even when the league was already found — match instead.
            Some(id) if !id.is_empty() => match leagues.iter().find(|l| l.league_id == id) {
                Some(l) => l.clone(),
                None => client.league(id).await?,
            },
            _ => leagues
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("user {} has no NFL leagues in {}", user.user_id, state.season))?,
        };
        let scoring_key = scoring_key_for(&league);
        Ok(Self {
            client,
            league_id: league.league_id,
            my_user_id: user.user_id,
            season: state.season,
            scoring_key,
            teams_cache: RwLock::new(None),
        })
    }

    pub async fn discover_leagues(
        client: &SleeperClient,
        username_or_id: &str,
    ) -> Result<Vec<DiscoveredLeague>> {
        let state = client.state().await?;
        let user = client.user(username_or_id).await?;
        let leagues = client.user_leagues(&user.user_id, &state.season).await?;
        Ok(leagues
            .iter()
            .map(|l| DiscoveredLeague {
                league_id: l.league_id.clone(),
                name: l.name.clone(),
                season: l.season.clone(),
                total_rosters: l.total_rosters,
                scoring: scoring_label(l),
            })
            .collect())
    }

    pub async fn current_week(&self) -> Result<u8> {
        Ok(self.client.state().await?.week.max(1))
    }

    pub async fn league_settings(&self) -> Result<LeagueSettings> {
        let l = self.client.league(&self.league_id).await?;
        let mut counts: HashMap<Position, u32> = HashMap::new();
        for slot in &l.roster_positions {
            *counts.entry(Position::from_str(slot)).or_insert(0) += 1;
        }
        let order = [
            Position::QB, Position::RB, Position::WR, Position::TE,
            Position::FLEX, Position::SUPERFLEX, Position::DST, Position::K,
            Position::BENCH, Position::IR,
        ];
        Ok(LeagueSettings {
            scoring: scoring_label(&l),
            roster_slots: order
                .into_iter()
                .filter_map(|p| counts.get(&p).map(|c| (p, *c)))
                .collect(),
            team_count: l.total_rosters,
        })
    }

    fn build_player(
        &self,
        sp: &ApiPlayer,
        projections: Option<&HashMap<String, HashMap<String, f64>>>,
    ) -> Player {
        let name = sp
            .full_name
            .clone()
            .or_else(|| match (&sp.first_name, &sp.last_name) {
                (Some(f), Some(l)) => Some(format!("{f} {l}")),
                _ => None,
            })
            .unwrap_or_else(|| sp.player_id.clone());
        let projected = projections
            .and_then(|m| m.get(&sp.player_id))
            .and_then(|stats| stats.get(&self.scoring_key).or_else(|| stats.get("pts_ppr")))
            .copied()
            .unwrap_or(0.0) as f32;
        let mut news = Vec::new();
        if let Some(n) = &sp.injury_notes {
            if !n.trim().is_empty() {
                news.push(n.clone());
            }
        }
        Player {
            id: sp.player_id.clone(),
            name,
            position: sp.position.as_deref().map(Position::from_str).unwrap_or(Position::Unknown),
            roster_slot: Position::BENCH,
            team: sp.team.clone().unwrap_or_default(),
            projected_points: projected,
            avg_points: 0.0,
            status: sp
                .injury_status
                .as_deref()
                .map(PlayerStatus::from_str)
                .unwrap_or(PlayerStatus::Healthy),
            opponent: None,
            bye_week: None,
            news,
        }
    }

    /// Drop the short-TTL rosters/users snapshot so the next call re-fetches.
    /// Used by manual refresh paths and simulations that advance time quickly.
    pub fn invalidate_team_cache(&self) {
        *self.teams_cache.write() = None;
    }

    async fn team_names(&self) -> Result<TeamsSnapshot> {
        if let Some((at, snap)) = self.teams_cache.read().clone() {
            if at.elapsed() < TEAMS_CACHE_TTL {
                return Ok((*snap).clone());
            }
        }
        let rosters = self.client.league_rosters(&self.league_id).await?;
        let users = self.client.league_users(&self.league_id).await?;
        let mut names = HashMap::new();
        for r in &rosters {
            let owner = r
                .owner_id
                .as_ref()
                .and_then(|oid| users.iter().find(|u| &u.user_id == oid));
            let name = owner
                .and_then(|u| u.metadata.as_ref())
                .and_then(|m| m.get("team_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| owner.and_then(|u| u.display_name.clone()))
                .unwrap_or_else(|| format!("Team {}", r.roster_id));
            names.insert(r.roster_id, name);
        }
        let snap: TeamsSnapshot = (rosters, names, users);
        *self.teams_cache.write() = Some((std::time::Instant::now(), Arc::new(snap.clone())));
        Ok(snap)
    }

    /// Build all rosters with projection-backed player points.
    pub async fn all_rosters(&self, week: u8) -> Result<Vec<Roster>> {
        let (rosters, names, users) = self.team_names().await?;
        let players = self.client.all_players().await?;
        let proj = self.client.projections(&self.season, week).await.ok();
        Ok(rosters
            .iter()
            .map(|r| {
                let starters: HashSet<&str> = r
                    .starters
                    .as_ref()
                    .map(|s| s.iter().map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                let reserve: HashSet<&str> = r
                    .reserve
                    .as_ref()
                    .map(|s| s.iter().map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                let mut ps = Vec::new();
                if let Some(ids) = &r.players {
                    for id in ids {
                        if id == "0" {
                            continue;
                        }
                        if let Some(sp) = players.get(id) {
                            let mut p = self.build_player(sp, proj.as_deref());
                            if starters.contains(id.as_str()) {
                                p.roster_slot = p.position;
                            } else if reserve.contains(id.as_str()) {
                                p.roster_slot = Position::IR;
                            }
                            ps.push(p);
                        }
                    }
                }
                let owner = r
                    .owner_id
                    .as_ref()
                    .and_then(|oid| users.iter().find(|u| &u.user_id == oid));
                let (wins, losses, ties, pf, pa) = roster_record(r);
                Roster {
                    team_id: r.roster_id.to_string(),
                    team_name: names.get(&r.roster_id).cloned().unwrap_or_default(),
                    owner: owner.and_then(|u| u.display_name.clone()),
                    players: ps,
                    wins,
                    losses,
                    ties,
                    points_for: pf,
                    points_against: pa,
                }
            })
            .collect())
    }

    pub async fn my_roster(&self, week: u8) -> Result<Roster> {
        let (rosters, _, _) = self.team_names().await?;
        let mine = rosters
            .iter()
            .find(|r| r.owner_id.as_deref() == Some(self.my_user_id.as_str()))
            .ok_or_else(|| anyhow!("no roster owned by user {} in this league", self.my_user_id))?;
        let all = self.all_rosters(week).await?;
        all.into_iter()
            .find(|r| r.team_id == mine.roster_id.to_string())
            .ok_or_else(|| anyhow!("roster resolution failed"))
    }

    pub async fn matchups(&self, week: u8) -> Result<Vec<Matchup>> {
        let raw = self.client.league_matchups(&self.league_id, week).await?;
        let (_, names, _) = self.team_names().await?;
        let proj = self.client.projections(&self.season, week).await.ok();
        let players = self.client.all_players().await.ok();
        let projected_for = |m: &ApiMatchup| -> f32 {
            let (Some(proj), Some(_players)) = (&proj, &players) else {
                return 0.0;
            };
            m.starters
                .as_ref()
                .map(|s| {
                    s.iter()
                        .filter(|id| *id != "0")
                        .filter_map(|id| proj.get(id))
                        .filter_map(|stats| {
                            stats.get(&self.scoring_key).or_else(|| stats.get("pts_ppr"))
                        })
                        .sum::<f64>() as f32
                })
                .unwrap_or(0.0)
        };
        let mut by_match: HashMap<u32, Vec<&ApiMatchup>> = HashMap::new();
        for m in &raw {
            if let Some(mid) = m.matchup_id {
                by_match.entry(mid).or_default().push(m);
            }
        }
        let mut out = Vec::new();
        for (_, pair) in by_match {
            if pair.len() == 2 {
                out.push(Matchup {
                    week,
                    home_team: names.get(&pair[0].roster_id).cloned().unwrap_or_default(),
                    away_team: names.get(&pair[1].roster_id).cloned().unwrap_or_default(),
                    home_projected: projected_for(pair[0]),
                    away_projected: projected_for(pair[1]),
                    home_score: pair[0].points,
                    away_score: pair[1].points,
                });
            }
        }
        out.sort_by(|a, b| a.home_team.cmp(&b.home_team));
        Ok(out)
    }

    /// Free agents at every position, projection-ranked.
    pub async fn free_agents(&self, position: Option<Position>, week: u8, limit: usize) -> Result<Vec<Player>> {
        let players = self.client.all_players().await?;
        let (rosters, _, _) = self.team_names().await?;
        let proj = self.client.projections(&self.season, week).await.ok();
        let mut taken: HashSet<&str> = HashSet::new();
        for r in &rosters {
            if let Some(ids) = &r.players {
                taken.extend(ids.iter().map(|s| s.as_str()));
            }
        }
        let mut out: Vec<Player> = players
            .values()
            .filter(|sp| !sp.player_id.is_empty() && !taken.contains(sp.player_id.as_str()))
            .filter(|sp| sp.team.as_deref().map(|t| !t.is_empty()).unwrap_or(false))
            .filter(|sp| match position {
                Some(p) => sp.position.as_deref().map(|s| Position::from_str(s) == p).unwrap_or(false),
                None => true,
            })
            .map(|sp| self.build_player(sp, proj.as_deref()))
            .collect();
        out.sort_by(|a, b| {
            b.projected_points
                .partial_cmp(&a.projected_points)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Trending adds/drops resolved to domain players.
    pub async fn trending_players(&self, direction: TrendDirection, limit: u32) -> Result<Vec<TrendingPlayer>> {
        let raw = self.client.trending(direction, 24, limit).await?;
        let players = self.client.all_players().await?;
        let week = self.current_week().await.unwrap_or(1);
        let proj = self.client.projections(&self.season, week).await.ok();
        Ok(raw
            .iter()
            .filter_map(|t| {
                players.get(&t.player_id).map(|sp| TrendingPlayer {
                    player: self.build_player(sp, proj.as_deref()),
                    count: t.count,
                    direction,
                })
            })
            .collect())
    }

    /// League transactions for the last `weeks_back` weeks, newest first.
    pub async fn recent_transactions(&self, week: u8, weeks_back: u8) -> Result<Vec<Transaction>> {
        let (_, names, _) = self.team_names().await?;
        let players = self.client.all_players().await?;
        let player_name = |id: &str| -> String {
            players
                .get(id)
                .and_then(|p| p.full_name.clone())
                .unwrap_or_else(|| id.to_string())
        };
        let mut out = Vec::new();
        let start = week.saturating_sub(weeks_back.saturating_sub(1)).max(1);
        for w in (start..=week).rev() {
            let txs = match self.client.transactions(&self.league_id, w).await {
                Ok(t) => t,
                Err(_) => continue,
            };
            for tx in txs {
                let team_of = |rid: u32| names.get(&rid).cloned().unwrap_or_else(|| format!("Team {rid}"));
                let teams: Vec<String> = tx
                    .roster_ids
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(team_of)
                    .collect();
                let adds = tx
                    .adds
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(pid, rid)| (player_name(&pid), team_of(rid)))
                    .collect();
                let drops = tx
                    .drops
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(pid, rid)| (player_name(&pid), team_of(rid)))
                    .collect();
                let waiver_bid = tx
                    .settings
                    .as_ref()
                    .and_then(|s| s.get("waiver_bid"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let traded_picks = tx
                    .draft_picks
                    .clone()
                    .unwrap_or_default()
                    .iter()
                    .map(|p| {
                        format!(
                            "{} R{} pick ({} → {})",
                            p.season,
                            p.round,
                            p.previous_owner_id.map(team_of).unwrap_or_default(),
                            p.owner_id.map(team_of).unwrap_or_default()
                        )
                    })
                    .collect();
                out.push(Transaction {
                    id: tx.transaction_id,
                    kind: tx.kind,
                    status: tx.status,
                    week: tx.leg.unwrap_or(w),
                    teams,
                    adds,
                    drops,
                    waiver_bid,
                    traded_picks,
                });
            }
        }
        Ok(out)
    }

    pub async fn traded_picks(&self) -> Result<Vec<TradedPick>> {
        let raw = self.client.traded_picks(&self.league_id).await?;
        let (_, names, _) = self.team_names().await?;
        let team_of = |rid: Option<u32>| {
            rid.and_then(|r| names.get(&r).cloned()).unwrap_or_default()
        };
        Ok(raw
            .iter()
            .map(|p| TradedPick {
                season: p.season.clone(),
                round: p.round,
                original_owner: team_of(p.roster_id),
                current_owner: team_of(p.owner_id),
            })
            .collect())
    }

    pub async fn playoff_bracket(&self) -> Result<(Vec<BracketMatch>, Vec<BracketMatch>)> {
        let (_, names, _) = self.team_names().await?;
        let resolve = |v: &Option<serde_json::Value>| -> Option<String> {
            let v = v.as_ref()?;
            if let Some(rid) = v.as_u64() {
                return names.get(&(rid as u32)).cloned();
            }
            // Unresolved slots come as {"w": N} / {"l": N} — "winner/loser of match N".
            if let Some(obj) = v.as_object() {
                if let Some(m) = obj.get("w").and_then(|x| x.as_u64()) {
                    return Some(format!("Winner of M{m}"));
                }
                if let Some(m) = obj.get("l").and_then(|x| x.as_u64()) {
                    return Some(format!("Loser of M{m}"));
                }
            }
            None
        };
        let convert = |raw: Vec<ApiBracketMatch>| -> Vec<BracketMatch> {
            raw.iter()
                .map(|m| BracketMatch {
                    round: m.r,
                    match_id: m.m,
                    team1: resolve(&m.t1),
                    team2: resolve(&m.t2),
                    winner: m.w.and_then(|rid| names.get(&rid).cloned()),
                    placing: m.p,
                })
                .collect()
        };
        let winners = self.winners().await.unwrap_or_default();
        let losers = self.client.losers_bracket(&self.league_id).await.unwrap_or_default();
        Ok((convert(winners), convert(losers)))
    }

    async fn winners(&self) -> Result<Vec<ApiBracketMatch>> {
        self.client.winners_bracket(&self.league_id).await
    }

    pub async fn draft_state(&self) -> Result<DraftState> {
        let drafts = self.client.league_drafts(&self.league_id).await?;
        let d = drafts
            .first()
            .ok_or_else(|| anyhow!("league has no draft"))?;
        let draft = self.client.draft(&d.draft_id).await?;
        let picks = self.client.draft_picks(&d.draft_id).await?;
        let players = self.client.all_players().await?;
        let (_, names, users) = self.team_names().await?;
        let total_rounds = draft.settings.get("rounds").and_then(|v| v.as_u64()).unwrap_or(15) as u32;
        let team_count = draft.settings.get("teams").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
        let pick_list: Vec<DraftPick> = picks
            .iter()
            .map(|p| {
                let team = p
                    .roster_id
                    .and_then(|rid| names.get(&rid).cloned())
                    .or_else(|| {
                        p.picked_by
                            .as_ref()
                            .and_then(|uid| users.iter().find(|u| &u.user_id == uid))
                            .and_then(|u| u.display_name.clone())
                    })
                    .unwrap_or_default();
                let player = players.get(&p.player_id);
                DraftPick {
                    pick_number: p.pick_no,
                    round: p.round,
                    team_id: p.roster_id.map(|r| r.to_string()).unwrap_or_default(),
                    team_name: team,
                    player_id: Some(p.player_id.clone()),
                    player_name: player.and_then(|sp| sp.full_name.clone()),
                    position: player
                        .and_then(|sp| sp.position.as_deref())
                        .map(Position::from_str),
                }
            })
            .collect();
        // On-the-clock team from draft_order + snake math.
        // Don't assume the API returns picks sorted — take the max pick_no.
        let next_pick = pick_list
            .iter()
            .map(|p| p.pick_number)
            .max()
            .map(|n| n + 1)
            .unwrap_or(1);
        // Third-round reversal: rounds >= reversal_round flip direction once
        // more (round R repeats round R-1's direction, then snaking resumes).
        let reversal = draft
            .settings
            .get("reversal_round")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let on_clock_user = draft.draft_order.as_ref().and_then(|order| {
            let idx0 = (next_pick - 1) % team_count.max(1);
            let round0 = (next_pick - 1) / team_count.max(1);
            let parity = if reversal > 0 && round0 + 1 >= reversal {
                (round0 + 1) % 2
            } else {
                round0 % 2
            };
            let slot = if parity == 1 {
                team_count.saturating_sub(1).saturating_sub(idx0)
            } else {
                idx0
            } + 1;
            order
                .iter()
                .find(|(_, s)| **s == slot)
                .map(|(uid, _)| uid.clone())
        });
        let on_clock = on_clock_user
            .as_ref()
            .and_then(|uid| users.iter().find(|u| &u.user_id == uid))
            .and_then(|u| u.display_name.clone());
        Ok(DraftState {
            picks: pick_list,
            current_pick: next_pick,
            on_the_clock_team: on_clock,
            on_the_clock_user_id: on_clock_user,
            total_rounds,
            team_count,
            completed: draft.status == "complete",
        })
    }
}

fn roster_record(r: &ApiRoster) -> (u32, u32, u32, f32, f32) {
    r.settings
        .as_ref()
        .map(|s| {
            let g = |k: &str| s.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let f = |k: &str| s.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            (
                g("wins") as u32,
                g("losses") as u32,
                g("ties") as u32,
                f("fpts") + f("fpts_decimal") / 100.0,
                f("fpts_against") + f("fpts_against_decimal") / 100.0,
            )
        })
        .unwrap_or((0, 0, 0, 0.0, 0.0))
}

fn scoring_key_for(l: &ApiLeague) -> String {
    let rec = l
        .scoring_settings
        .get("rec")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if rec >= 0.99 {
        "pts_ppr".into()
    } else if rec >= 0.49 {
        "pts_half_ppr".into()
    } else {
        "pts_std".into()
    }
}

fn scoring_label(l: &ApiLeague) -> String {
    match scoring_key_for(l).as_str() {
        "pts_ppr" => "ppr".into(),
        "pts_half_ppr" => "half_ppr".into(),
        _ => "standard".into(),
    }
}
