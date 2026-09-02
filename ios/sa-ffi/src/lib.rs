//! C ABI bridge exposing the sleeper-agent core to the SwiftUI iOS app.
//!
//! Everything crosses the boundary as JSON through a single `sa_request`
//! entry point rather than as fifty typed C functions. That keeps the unsafe
//! surface tiny — four functions, one of which just frees a string — and lets
//! the Swift side model each payload as a plain `Codable` struct. Adding a
//! feature means adding a `Request` variant, not a new symbol to export.
//!
//! Every call is async: `sa_request` returns immediately and the reply arrives
//! on the callback, off the UI thread.
//!
//! Note there is deliberately no `claude-cli` path here. iOS forbids spawning
//! subprocesses, so the phone can only reach Claude through the HTTP API with
//! a key; `Config::for_ios` enforces that rather than letting the backend
//! silently resolve to something that cannot run.

use serde::{Deserialize, Serialize};
use sleeper_agent::{
    anthropic::Anthropic,
    api::{LeagueSession, SleeperClient},
    config::Config,
    draft, league, lineup,
    news::{NewsFetcher, NewsItem},
    player_detail, scheduler,
    scheduler::AppData,
    strategy::Strategy,
    trade,
    types::*,
    waiver,
};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    /// Connect (or reconnect) to a league and do a first refresh.
    Connect {
        username: String,
        #[serde(default)]
        league_id: Option<String>,
    },
    /// Leagues this username belongs to, for the settings picker.
    DiscoverLeagues { username: String },
    /// Re-fetch everything into the snapshot.
    Refresh,
    /// The current snapshot without re-fetching.
    Snapshot,
    Lineup,
    Waiver,
    TradeScan {
        count: usize,
        multi_tier: bool,
        horizon: String,
        #[serde(default)]
        send_hints: Vec<String>,
        #[serde(default)]
        want_positions: Vec<String>,
    },
    TradeAnalyze {
        partner: String,
        send: Vec<String>,
        receive: Vec<String>,
    },
    TrendWhy {
        player_id: String,
        direction: String,
    },
    DraftSuggest,
    /// Position-by-position ranking of my team against the league.
    LeagueRanks,
    /// Everything the detail panel shows for one player.
    PlayerDetail { player_id: String },
    GetConfig,
    SaveConfig { config: ConfigDto },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Response {
    Ok { ok: bool, data: serde_json::Value },
    Err { ok: bool, error: String },
}

impl Response {
    fn ok(data: serde_json::Value) -> Self {
        Self::Ok { ok: true, data }
    }
    fn err(e: impl std::fmt::Display) -> Self {
        Self::Err {
            ok: false,
            error: e.to_string(),
        }
    }
}

/// The settings the phone can edit. Deliberately a subset of `Config`: the
/// daemon and webhook settings belong to the headless deployment, not here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDto {
    pub username: String,
    pub league_id: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub strategy: String,
    pub news_sources: Vec<String>,
    /// True when the key came from the environment and must not be persisted.
    #[serde(default)]
    pub api_key_from_env: bool,
}

impl ConfigDto {
    fn from_config(c: &Config) -> Self {
        Self {
            username: c.sleeper.username.clone(),
            league_id: c.sleeper.league_id.clone(),
            api_key: c.anthropic.api_key.clone(),
            model: c.anthropic.model.clone(),
            max_tokens: c.anthropic.max_tokens,
            strategy: strategy_wire(c.settings.strategy),
            news_sources: c.settings.news_sources.clone(),
            api_key_from_env: c.api_key_from_env,
        }
    }

    fn apply_to(&self, c: &mut Config) {
        c.sleeper.username = self.username.clone();
        c.sleeper.league_id = self.league_id.clone();
        c.anthropic.api_key = self.api_key.clone();
        c.anthropic.model = self.model.clone();
        c.anthropic.max_tokens = self.max_tokens;
        c.settings.strategy = parse_strategy(&self.strategy);
        if !self.news_sources.is_empty() {
            c.settings.news_sources = self.news_sources.clone();
        }
    }
}

/// Wire form of a strategy.
///
/// Deliberately serde's representation, not `Strategy::label()` — the label is
/// a display string ("High Stakes / High Rewards") and using it as a key means
/// the value fails to parse on the way back in, silently resetting the user's
/// choice to the default.
fn strategy_wire(s: Strategy) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "balanced".into())
}

fn parse_strategy(s: &str) -> Strategy {
    let key = s.trim().to_lowercase().replace([' ', '-'], "_");
    serde_json::from_value(serde_json::Value::String(key)).unwrap_or(Strategy::Balanced)
}

/// The whole app snapshot, flattened for Swift.
///
/// `AppData` itself is not serialisable (it carries an `Instant`), and the UI
/// wants a couple of derived fields anyway, so this is an explicit projection
/// rather than a derive on the domain type.
#[derive(Debug, Serialize)]
struct SnapshotDto {
    week: u8,
    season: String,
    team_name: String,
    roster: Option<Roster>,
    all_rosters: Vec<Roster>,
    settings: Option<LeagueSettings>,
    matchups: Vec<Matchup>,
    news: Vec<NewsItem>,
    draft: Option<DraftState>,
    transactions: Vec<Transaction>,
    trending_add: Vec<TrendingPlayer>,
    trending_drop: Vec<TrendingPlayer>,
    traded_picks: Vec<TradedPick>,
    winners_bracket: Vec<BracketMatch>,
    losers_bracket: Vec<BracketMatch>,
    last_error: Option<String>,
    /// Season the accuracy table covers — not always the current one.
    perf_season: String,
    has_perf: bool,
}

impl SnapshotDto {
    fn from(d: &AppData) -> Self {
        Self {
            week: d.week,
            season: d.season.clone(),
            team_name: d.roster.as_ref().map(|r| r.team_name.clone()).unwrap_or_default(),
            roster: d.roster.clone(),
            all_rosters: d.all_rosters.clone(),
            settings: d.settings.clone(),
            matchups: d.matchups.clone(),
            news: d.news.clone(),
            draft: d.draft.clone(),
            transactions: d.transactions.clone(),
            trending_add: d.trending_add.clone(),
            trending_drop: d.trending_drop.clone(),
            traded_picks: d.traded_picks.clone(),
            winners_bracket: d.winners_bracket.clone(),
            losers_bracket: d.losers_bracket.clone(),
            last_error: d.last_error.clone(),
            perf_season: d.perf.season.clone(),
            has_perf: !d.perf.is_empty(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PosRankDto {
    position: String,
    starters: f32,
    bench: f32,
    rank_starters: u32,
    rank_bench: u32,
    teams: u32,
    league_avg_starters: f32,
    /// 0..1, 1 = best in league. What the radar plots.
    starter_score: f32,
    vs_average: f32,
    is_weakness: bool,
    is_strength: bool,
}

#[derive(Debug, Serialize)]
struct PlayerDetailDto {
    player: Player,
    owner: Option<String>,
    upcoming: Vec<player_detail::UpcomingGame>,
    perf: Option<player_detail::PerfRecord>,
    perf_season: String,
    /// Label/value pairs, already filtered to what matters for the position.
    totals: Vec<(String, f64)>,
    trending_add: Option<u64>,
    trending_drop: Option<u64>,
    news: Vec<String>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

struct Inner {
    cfg: Config,
    client: Arc<SleeperClient>,
    session: Option<Arc<LeagueSession>>,
    anthropic: Option<Arc<Anthropic>>,
    news: Option<Arc<NewsFetcher>>,
}

pub struct Engine {
    rt: tokio::runtime::Runtime,
    inner: Arc<tokio::sync::RwLock<Inner>>,
    data: Arc<parking_lot::RwLock<AppData>>,
}

impl Engine {
    fn new(config_dir: PathBuf, cache_dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&cache_dir)?;
        // The core reads the cache location from the environment; on iOS the
        // only writable place is the app sandbox, so point it there.
        std::env::set_var("SA_CACHE_DIR", &cache_dir);

        let path = config_dir.join("config.yaml");
        let cfg = match Config::load(&path) {
            Ok(c) => c,
            Err(_) => {
                // First launch: start from defaults rather than failing, so
                // the user lands on Settings instead of an error screen.
                let mut c = Config::for_ios();
                c.path = path.clone();
                c.base_dir = config_dir.clone();
                let _ = c.save();
                c
            }
        };

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;

        Ok(Self {
            rt,
            inner: Arc::new(tokio::sync::RwLock::new(Inner {
                client: Arc::new(SleeperClient::new()?),
                session: None,
                anthropic: None,
                news: None,
                cfg,
            })),
            data: Arc::new(parking_lot::RwLock::new(AppData::default())),
        })
    }
}

/// Rebuild the Claude client from current config. Returns a clear error rather
/// than a resolved-but-unusable backend, since the CLI cannot exist here.
async fn ensure_anthropic(inner: &Arc<tokio::sync::RwLock<Inner>>) -> anyhow::Result<Arc<Anthropic>> {
    {
        let g = inner.read().await;
        if let Some(a) = &g.anthropic {
            return Ok(a.clone());
        }
    }
    let mut g = inner.write().await;
    let ctx = g.cfg.load_context().unwrap_or(None);
    if g.cfg.anthropic.api_key.trim().is_empty() {
        anyhow::bail!(
            "No Anthropic API key set. iOS cannot run the Claude CLI, so a key is \
             required for AI features — add one in Settings."
        );
    }
    let a = Arc::new(Anthropic::new(g.cfg.anthropic.clone())?.with_context(ctx));
    g.anthropic = Some(a.clone());
    Ok(a)
}

async fn session_of(inner: &Arc<tokio::sync::RwLock<Inner>>) -> anyhow::Result<Arc<LeagueSession>> {
    let g = inner.read().await;
    g.session
        .clone()
        .ok_or_else(|| anyhow::anyhow!("not connected to a league yet"))
}

async fn news_of(inner: &Arc<tokio::sync::RwLock<Inner>>) -> Arc<NewsFetcher> {
    let mut g = inner.write().await;
    if let Some(n) = &g.news {
        return n.clone();
    }
    let f = Arc::new(
        NewsFetcher::new(g.cfg.settings.news_sources.clone())
            .unwrap_or_else(|_| NewsFetcher::new(vec![]).expect("empty fetcher")),
    );
    g.news = Some(f.clone());
    f
}

async fn handle(
    req: Request,
    inner: Arc<tokio::sync::RwLock<Inner>>,
    data: Arc<parking_lot::RwLock<AppData>>,
) -> anyhow::Result<serde_json::Value> {
    match req {
        Request::Connect { username, league_id } => {
            let client = inner.read().await.client.clone();
            let league = league_id.filter(|s| !s.is_empty());
            let session =
                Arc::new(LeagueSession::connect(client, &username, league.as_deref()).await?);
            {
                let mut g = inner.write().await;
                g.cfg.sleeper.username = username;
                if let Some(l) = &league {
                    g.cfg.sleeper.league_id = l.clone();
                }
                let _ = g.cfg.save();
                g.session = Some(session.clone());
            }
            let fetcher = news_of(&inner).await;
            scheduler::refresh_once(&session, &fetcher, &data).await?;
            Ok(serde_json::to_value(SnapshotDto::from(&data.read()))?)
        }

        Request::DiscoverLeagues { username } => {
            let client = inner.read().await.client.clone();
            let leagues = LeagueSession::discover_leagues(&client, &username).await?;
            Ok(serde_json::to_value(leagues)?)
        }

        Request::Refresh => {
            let session = session_of(&inner).await?;
            let fetcher = news_of(&inner).await;
            scheduler::refresh_once(&session, &fetcher, &data).await?;
            Ok(serde_json::to_value(SnapshotDto::from(&data.read()))?)
        }

        Request::Snapshot => Ok(serde_json::to_value(SnapshotDto::from(&data.read()))?),

        Request::Lineup => {
            let session = session_of(&inner).await?;
            let anthropic = ensure_anthropic(&inner).await?;
            let strat = inner.read().await.cfg.settings.strategy;
            let (roster, settings, matchups, week, news) = {
                let d = data.read();
                (
                    d.roster.clone(),
                    d.settings.clone(),
                    d.matchups.clone(),
                    d.week,
                    d.news.clone(),
                )
            };
            let week = if week == 0 { session.current_week().await? } else { week };
            let roster = match roster {
                Some(r) => r,
                None => session.my_roster(week).await?,
            };
            let settings = match settings {
                Some(s) => s,
                None => session.league_settings().await?,
            };
            let l =
                lineup::ai_optimize(&anthropic, &roster, &settings, &matchups, &news, strat, week)
                    .await?;
            Ok(serde_json::to_value(l)?)
        }

        Request::Waiver => {
            let session = session_of(&inner).await?;
            let anthropic = ensure_anthropic(&inner).await?;
            let strat = inner.read().await.cfg.settings.strategy;
            let news = data.read().news.clone();
            let r = waiver::analyze(&session, &anthropic, strat, &news, 300).await?;
            Ok(serde_json::to_value(r)?)
        }

        Request::TradeScan {
            count,
            multi_tier,
            horizon,
            send_hints,
            want_positions,
        } => {
            let session = session_of(&inner).await?;
            let anthropic = ensure_anthropic(&inner).await?;
            let strat = inner.read().await.cfg.settings.strategy;
            let (news, week) = {
                let d = data.read();
                (d.news.clone(), d.week)
            };
            let week = if week == 0 { session.current_week().await? } else { week };
            let me = session.my_roster(week).await?;
            let all = session.all_rosters(week).await?;
            let opts = trade::SuggestOptions {
                count,
                multi_tier,
                send_hints,
                want_positions,
                horizon: if horizon == "this_week" {
                    trade::Horizon::ThisWeek
                } else {
                    trade::Horizon::RestOfSeason
                },
                week,
            };
            let (ideas, raw) =
                trade::suggest_ideas(&anthropic, &me, &all, strat, &news, &opts).await?;
            Ok(serde_json::json!({ "ideas": ideas, "raw": raw }))
        }

        Request::TradeAnalyze {
            partner,
            send,
            receive,
        } => {
            let session = session_of(&inner).await?;
            let anthropic = ensure_anthropic(&inner).await?;
            let strat = inner.read().await.cfg.settings.strategy;
            let (news, week) = {
                let d = data.read();
                (d.news.clone(), d.week)
            };
            let week = if week == 0 { session.current_week().await? } else { week };
            let me = session.my_roster(week).await?;
            let all = session.all_rosters(week).await?;
            let a = trade::analyze(
                &anthropic, &me, &partner, &send, &receive, &all, strat, &news,
            )
            .await?;
            Ok(serde_json::to_value(a)?)
        }

        Request::TrendWhy {
            player_id,
            direction,
        } => {
            let anthropic = ensure_anthropic(&inner).await?;
            let strat = inner.read().await.cfg.settings.strategy;
            let (player, count, roster, news) = {
                let d = data.read();
                let feed = if direction == "drop" {
                    &d.trending_drop
                } else {
                    &d.trending_add
                };
                let t = feed
                    .iter()
                    .find(|t| t.player.id == player_id)
                    .ok_or_else(|| anyhow::anyhow!("player not in the trending feed"))?;
                (
                    t.player.clone(),
                    t.count,
                    d.roster.clone(),
                    d.news.clone(),
                )
            };
            let roster = roster.ok_or_else(|| anyhow::anyhow!("roster not loaded yet"))?;
            let dir = if direction == "drop" {
                TrendDirection::Drop
            } else {
                TrendDirection::Add
            };
            let text = waiver::explain_trending(
                &anthropic, &player, count, dir, &roster, &news, strat,
            )
            .await?;
            Ok(serde_json::json!({ "text": text }))
        }

        Request::DraftSuggest => {
            let session = session_of(&inner).await?;
            let anthropic = ensure_anthropic(&inner).await?;
            let strat = inner.read().await.cfg.settings.strategy;
            let my_team_name = data
                .read()
                .roster
                .as_ref()
                .map(|r| r.team_name.clone())
                .unwrap_or_default();
            let mgr = draft::DraftManager {
                session: &session,
                anthropic: &anthropic,
                strategy: strat,
                my_team_name,
            };
            let state = mgr.snapshot().await?;
            let news = data.read().news.clone();
            let s = mgr.ask_claude(&state, &news).await?;
            Ok(serde_json::to_value(s)?)
        }

        Request::LeagueRanks => {
            let (all, team) = {
                let d = data.read();
                (
                    d.all_rosters.clone(),
                    d.roster.as_ref().map(|r| r.team_name.clone()).unwrap_or_default(),
                )
            };
            if all.len() < 2 {
                anyhow::bail!("league rosters not loaded yet");
            }
            let ranks: Vec<PosRankDto> = league::rank_team(&all, &team)
                .into_iter()
                .map(|r| PosRankDto {
                    position: r.position.to_string(),
                    starters: r.starters,
                    bench: r.bench,
                    rank_starters: r.rank_starters,
                    rank_bench: r.rank_bench,
                    teams: r.teams,
                    league_avg_starters: r.league_avg_starters,
                    starter_score: r.starter_score(),
                    vs_average: r.vs_average(),
                    is_weakness: r.is_weakness(),
                    is_strength: r.is_strength(),
                })
                .collect();
            Ok(serde_json::to_value(ranks)?)
        }

        Request::PlayerDetail { player_id } => {
            let d = data.read();
            let player = d
                .all_rosters
                .iter()
                .flat_map(|r| r.players.iter())
                .chain(d.trending_add.iter().map(|t| &t.player))
                .chain(d.trending_drop.iter().map(|t| &t.player))
                .find(|p| p.id == player_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("player not found in the current snapshot"))?;

            let owner = d
                .all_rosters
                .iter()
                .find(|r| r.players.iter().any(|p| p.id == player_id))
                .map(|r| r.team_name.clone());
            let rec = d.perf.get(&player_id).cloned();
            let totals = rec
                .as_ref()
                .map(|r| player_detail::notable_stats(&r.totals, &player.position.to_string()))
                .unwrap_or_default();
            let mut headlines: Vec<String> = player.news.clone();
            for n in &d.news {
                if n.title.contains(&player.name) && !headlines.contains(&n.title) {
                    headlines.push(n.title.clone());
                }
            }
            let dto = PlayerDetailDto {
                upcoming: player_detail::upcoming_for_team(
                    &d.schedule,
                    &player.team,
                    d.week,
                    5,
                ),
                owner,
                perf: rec,
                perf_season: d.perf.season.clone(),
                totals,
                trending_add: d
                    .trending_add
                    .iter()
                    .find(|t| t.player.id == player_id)
                    .map(|t| t.count),
                trending_drop: d
                    .trending_drop
                    .iter()
                    .find(|t| t.player.id == player_id)
                    .map(|t| t.count),
                news: headlines,
                player,
            };
            Ok(serde_json::to_value(dto)?)
        }

        Request::GetConfig => {
            let g = inner.read().await;
            Ok(serde_json::to_value(ConfigDto::from_config(&g.cfg))?)
        }

        Request::SaveConfig { config } => {
            let mut g = inner.write().await;
            config.apply_to(&mut g.cfg);
            g.cfg.save()?;
            // Credentials and feeds may have changed; rebuild lazily.
            g.anthropic = None;
            g.news = None;
            Ok(serde_json::to_value(ConfigDto::from_config(&g.cfg))?)
        }
    }
}

// ---------------------------------------------------------------------------
// C ABI
// ---------------------------------------------------------------------------

/// Read a C string into Rust, or return None when null/invalid UTF-8.
///
/// # Safety
/// `p` must be null or a valid NUL-terminated C string.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(str::to_owned)
}

/// Create the engine. Returns null on failure.
///
/// # Safety
/// Both arguments must be valid NUL-terminated C strings. The returned pointer
/// must be released with `sa_engine_free` and not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn sa_engine_new(
    config_dir: *const c_char,
    cache_dir: *const c_char,
) -> *mut Engine {
    let (Some(cfg_dir), Some(cache)) = (cstr(config_dir), cstr(cache_dir)) else {
        return std::ptr::null_mut();
    };
    match Engine::new(PathBuf::from(cfg_dir), PathBuf::from(cache)) {
        Ok(e) => Box::into_raw(Box::new(e)),
        Err(e) => {
            tracing::error!(error = %e, "engine init failed");
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `engine` must have come from `sa_engine_new` and not been freed already.
#[no_mangle]
pub unsafe extern "C" fn sa_engine_free(engine: *mut Engine) {
    if !engine.is_null() {
        drop(Box::from_raw(engine));
    }
}

/// Run a request. Returns immediately; `cb` fires on a worker thread with the
/// JSON reply. The string handed to `cb` is freed as soon as `cb` returns, so
/// the callback must copy anything it wants to keep.
///
/// # Safety
/// `engine` must be a live pointer from `sa_engine_new`, `req_json` a valid C
/// string, and `ctx` must remain valid until `cb` has run.
#[no_mangle]
pub unsafe extern "C" fn sa_request(
    engine: *mut Engine,
    req_json: *const c_char,
    ctx: *mut c_void,
    cb: extern "C" fn(*mut c_void, *const c_char),
) {
    if engine.is_null() {
        reply(ctx, cb, &Response::err("engine is null"));
        return;
    }
    let engine = &*engine;
    let Some(raw) = cstr(req_json) else {
        reply(ctx, cb, &Response::err("request was not valid UTF-8"));
        return;
    };
    let req: Request = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(e) => {
            reply(ctx, cb, &Response::err(format!("bad request: {e}")));
            return;
        }
    };

    // Raw pointer is not Send; carry it across the spawn as an integer and
    // rebuild it in the callback. The Swift side guarantees the context
    // outlives the call.
    let ctx_addr = ctx as usize;
    let inner = engine.inner.clone();
    let data = engine.data.clone();
    engine.rt.spawn(async move {
        let resp = match handle(req, inner, data).await {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(e),
        };
        reply(ctx_addr as *mut c_void, cb, &resp);
    });
}

fn reply(ctx: *mut c_void, cb: extern "C" fn(*mut c_void, *const c_char), resp: &Response) {
    let json = serde_json::to_string(resp)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"failed to encode response"}"#.into());
    // A NUL inside the payload would truncate it; replace rather than drop the
    // whole reply.
    let c = CString::new(json).unwrap_or_else(|_| {
        CString::new(r#"{"ok":false,"error":"response contained a NUL byte"}"#).expect("static")
    });
    cb(ctx, c.as_ptr());
}

/// Build identity, so the phone can show which core it is running.
///
/// # Safety
/// The returned pointer is a static string and must not be freed.
#[no_mangle]
pub extern "C" fn sa_version() -> *const c_char {
    // Trailing NUL makes this a valid C string without an allocation.
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_from_their_json_shape() {
        let r: Request = serde_json::from_str(r#"{"op":"snapshot"}"#).unwrap();
        assert!(matches!(r, Request::Snapshot));

        let r: Request =
            serde_json::from_str(r#"{"op":"connect","username":"me","league_id":"1"}"#).unwrap();
        match r {
            Request::Connect { username, league_id } => {
                assert_eq!(username, "me");
                assert_eq!(league_id.as_deref(), Some("1"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn optional_request_fields_may_be_omitted() {
        let r: Request = serde_json::from_str(
            r#"{"op":"trade_scan","count":3,"multi_tier":false,"horizon":"this_week"}"#,
        )
        .unwrap();
        match r {
            Request::TradeScan {
                count,
                send_hints,
                want_positions,
                ..
            } => {
                assert_eq!(count, 3);
                assert!(send_hints.is_empty() && want_positions.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn responses_serialise_flat_for_swift() {
        let ok = serde_json::to_string(&Response::ok(serde_json::json!({"a":1}))).unwrap();
        assert_eq!(ok, r#"{"ok":true,"data":{"a":1}}"#);
        let err = serde_json::to_string(&Response::err("boom")).unwrap();
        assert_eq!(err, r#"{"ok":false,"error":"boom"}"#);
    }

    #[test]
    fn strategy_round_trips_through_its_label() {
        for s in [Strategy::Conservative, Strategy::Balanced, Strategy::HighStakes] {
            assert_eq!(parse_strategy(&strategy_wire(s)), s);
        }
    }

    #[test]
    fn the_human_label_is_not_the_wire_key() {
        // Using label() as the wire value silently reset the saved strategy,
        // because it does not parse back. Guard against reintroducing that.
        assert_eq!(strategy_wire(Strategy::HighStakes), "high_stakes");
        assert_ne!(Strategy::HighStakes.label(), strategy_wire(Strategy::HighStakes));
    }

    #[test]
    fn unknown_strategy_falls_back_to_balanced() {
        assert_eq!(parse_strategy("nonsense"), Strategy::Balanced);
    }

    #[test]
    fn config_dto_round_trips_without_losing_fields() {
        let mut c = Config::for_ios();
        let dto = ConfigDto {
            username: "u".into(),
            league_id: "42".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-6".into(),
            max_tokens: 1024,
            strategy: "high_stakes".into(),
            news_sources: vec!["https://example.com/f".into()],
            api_key_from_env: false,
        };
        dto.apply_to(&mut c);
        let back = ConfigDto::from_config(&c);
        assert_eq!(back.username, "u");
        assert_eq!(back.league_id, "42");
        assert_eq!(back.api_key, "sk-test");
        assert_eq!(back.max_tokens, 1024);
        assert_eq!(back.strategy, "high_stakes");
        assert_eq!(back.news_sources, vec!["https://example.com/f".to_string()]);
    }

    #[test]
    fn ios_config_pins_the_api_backend() {
        // The CLI cannot run on iOS, so "auto" must never be able to pick it.
        assert_eq!(Config::for_ios().anthropic.backend, "api");
    }
}
