//! Background refresh loop. One snapshot struct (`AppData`) feeds every UI.
//! Compared to the ESPN-era version, the Sleeper snapshot also carries
//! transactions, trending adds/drops, traded picks, and playoff brackets.

use crate::api::LeagueSession;
use crate::news::{self, NewsFetcher, NewsItem};
use crate::types::*;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

#[derive(Default, Clone)]
pub struct AppData {
    pub roster: Option<Roster>,
    pub all_rosters: Vec<Roster>,
    pub settings: Option<LeagueSettings>,
    pub week: u8,
    pub matchups: Vec<Matchup>,
    pub news: Vec<NewsItem>,
    pub draft: Option<DraftState>,
    pub transactions: Vec<Transaction>,
    pub trending_add: Vec<TrendingPlayer>,
    pub trending_drop: Vec<TrendingPlayer>,
    pub traded_picks: Vec<TradedPick>,
    pub winners_bracket: Vec<BracketMatch>,
    pub losers_bracket: Vec<BracketMatch>,
    pub last_refresh: Option<Instant>,
    pub last_error: Option<String>,
}

pub struct Scheduler {
    pub data: Arc<RwLock<AppData>>,
    pub notify: Arc<Notify>,
    pub interval: Duration,
}

impl Scheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            data: Arc::new(RwLock::new(AppData::default())),
            notify: Arc::new(Notify::new()),
            interval,
        }
    }

    pub fn poke(&self) {
        self.notify.notify_one();
    }

    pub fn spawn(
        &self,
        session: Arc<LeagueSession>,
        news_fetcher: Arc<NewsFetcher>,
    ) -> tokio::task::JoinHandle<()> {
        let data = self.data.clone();
        let notify = self.notify.clone();
        let interval = self.interval;
        tokio::spawn(async move {
            loop {
                if let Err(e) = refresh_once(&session, &news_fetcher, &data).await {
                    data.write().last_error = Some(e.to_string());
                }
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = notify.notified() => {}
                }
            }
        })
    }
}

/// Record a fetch failure and yield None so the previous snapshot value is kept.
fn keep<T>(res: anyhow::Result<T>, name: &str, errors: &mut Vec<String>) -> Option<T> {
    match res {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(section = name, error = %e, "refresh section failed");
            errors.push(format!("{name}: {e}"));
            None
        }
    }
}

pub async fn refresh_once(
    session: &LeagueSession,
    news_fetcher: &NewsFetcher,
    data: &Arc<RwLock<AppData>>,
) -> anyhow::Result<()> {
    let mut errors: Vec<String> = Vec::new();
    let (prev_week, draft_done) = {
        let g = data.read();
        (
            g.week,
            g.draft.as_ref().map(|d| d.completed).unwrap_or(false),
        )
    };

    let week_res = keep(session.current_week().await, "week", &mut errors);
    let week = week_res.unwrap_or(if prev_week == 0 { 1 } else { prev_week });

    let settings = keep(session.league_settings().await, "settings", &mut errors);
    let all_rosters = keep(session.all_rosters(week).await, "rosters", &mut errors);
    let roster = keep(session.my_roster(week).await, "my roster", &mut errors);
    let matchups = keep(session.matchups(week).await, "matchups", &mut errors);
    let transactions = keep(
        session.recent_transactions(week, 3).await,
        "transactions",
        &mut errors,
    );
    let trending_add = keep(
        session.trending_players(TrendDirection::Add, 25).await,
        "trending adds",
        &mut errors,
    );
    let trending_drop = keep(
        session.trending_players(TrendDirection::Drop, 25).await,
        "trending drops",
        &mut errors,
    );
    let traded_picks = keep(session.traded_picks().await, "traded picks", &mut errors);
    let brackets = keep(session.playoff_bracket().await, "brackets", &mut errors);
    // A completed draft never changes — skip the (player-DB-heavy) re-fetch.
    let draft = if draft_done {
        None
    } else {
        keep(session.draft_state().await, "draft", &mut errors)
    };

    let mut feed = news_fetcher.fetch_all(80).await;
    if let Some(r) = &roster {
        let names: Vec<String> = r.players.iter().map(|p| p.name.clone()).collect();
        let filtered = news::relevant_to(&feed, &names);
        if !filtered.is_empty() {
            feed = filtered;
        }
    }

    // Transient failures keep the previous snapshot value rather than wiping
    // the UI; the error string is surfaced via last_error.
    let mut g = data.write();
    g.week = week;
    if let Some(v) = settings {
        g.settings = Some(v);
    }
    if let Some(v) = roster {
        g.roster = Some(v);
    }
    if let Some(v) = all_rosters {
        g.all_rosters = v;
    }
    if let Some(v) = matchups {
        g.matchups = v;
    }
    if !feed.is_empty() {
        g.news = feed;
    }
    if let Some(v) = draft {
        g.draft = Some(v);
    }
    if let Some(v) = transactions {
        g.transactions = v;
    }
    if let Some(v) = trending_add {
        g.trending_add = v;
    }
    if let Some(v) = trending_drop {
        g.trending_drop = v;
    }
    if let Some(v) = traded_picks {
        g.traded_picks = v;
    }
    if let Some((w, l)) = brackets {
        g.winners_bracket = w;
        g.losers_bracket = l;
    }
    g.last_refresh = Some(Instant::now());
    g.last_error = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };
    Ok(())
}
