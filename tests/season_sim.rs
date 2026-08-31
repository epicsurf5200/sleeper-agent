//! Full end-to-end season simulation against a mock Sleeper + Anthropic API.
//!
//! Spins up an in-process HTTP server that serves a synthetic 12-team league
//! (players, draft, rosters, weekly projections, matchups, trending, brackets)
//! and canned Claude responses, then drives the REAL pipeline through:
//!   connect → live draft (on-the-clock + AI suggestions) → 14 regular-season
//!   weeks of AI lineups → waiver + trade analysis → playoffs → champion.
//!
//! Run with output: `cargo test --test season_sim -- --nocapture`

use parking_lot::Mutex;
use serde_json::{json, Value};
use sleeper_agent::api::{LeagueSession, SleeperClient};
use sleeper_agent::config::AnthropicConfig;
use sleeper_agent::strategy::Strategy;
use sleeper_agent::{anthropic, draft, lineup, trade, waiver};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEAMS: u32 = 12;
const ROUNDS: u32 = 15;
const REG_SEASON_WEEKS: u8 = 14;

// ---------------------------------------------------------------------------
// Deterministic RNG (no rand dependency)
// ---------------------------------------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    /// Uniform float in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next() % 10_000) as f64 / 10_000.0 * (hi - lo)
    }
}

// ---------------------------------------------------------------------------
// Simulated world
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakePlayer {
    id: String,
    name: String,
    pos: &'static str,
    nfl_team: String,
    skill: f64, // baseline weekly points
}

#[derive(Clone)]
struct Team {
    roster_id: u32,
    user_id: String,
    display_name: String,
    team_name: Option<String>,
    players: Vec<String>,
    wins: u32,
    losses: u32,
    fpts: f64,
    fpts_against: f64,
}

/// (matchup_id, roster_id, points, starters)
type MatchRow = (u32, u32, f32, Vec<String>);

struct Sim {
    week: u8,
    players: Vec<FakePlayer>,
    teams: Vec<Team>,
    draft_status: &'static str,
    draft_picks: Vec<(u32, u32, u32, String, String)>, // pick_no, round, roster_id, user_id, player_id
    projections: HashMap<String, f64>,
    matchup_hist: HashMap<u8, Vec<MatchRow>>,
    winners_bracket: Vec<Value>,
}

impl Sim {
    fn player(&self, id: &str) -> &FakePlayer {
        self.players.iter().find(|p| p.id == id).unwrap()
    }

    fn my_team(&self) -> &Team {
        &self.teams[0]
    }

    /// Best lineup for a roster by current projections (QB,2RB,2WR,TE,FLEX,K,DST).
    fn optimal_starters(&self, roster: &[String]) -> Vec<(String, String, f64)> {
        let mut by_pos: HashMap<&str, Vec<(&FakePlayer, f64)>> = HashMap::new();
        for id in roster {
            let p = self.player(id);
            let proj = self.projections.get(id).copied().unwrap_or(0.0);
            by_pos.entry(p.pos).or_default().push((p, proj));
        }
        for v in by_pos.values_mut() {
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        }
        let mut used: Vec<String> = Vec::new();
        let mut out: Vec<(String, String, f64)> = Vec::new(); // (slot, name, proj)
        let take = |slot: &str, pos: &str, used: &mut Vec<String>, out: &mut Vec<(String, String, f64)>, by_pos: &HashMap<&str, Vec<(&FakePlayer, f64)>>| {
            if let Some(list) = by_pos.get(pos) {
                if let Some((p, proj)) = list.iter().find(|(p, _)| !used.contains(&p.id)) {
                    used.push(p.id.clone());
                    out.push((slot.to_string(), p.name.clone(), *proj));
                    return;
                }
            }
            out.push((slot.to_string(), "(empty)".into(), 0.0));
        };
        take("QB", "QB", &mut used, &mut out, &by_pos);
        take("RB", "RB", &mut used, &mut out, &by_pos);
        take("RB", "RB", &mut used, &mut out, &by_pos);
        take("WR", "WR", &mut used, &mut out, &by_pos);
        take("WR", "WR", &mut used, &mut out, &by_pos);
        take("TE", "TE", &mut used, &mut out, &by_pos);
        // FLEX: best remaining RB/WR/TE
        let flex = ["RB", "WR", "TE"]
            .iter()
            .flat_map(|pos| by_pos.get(*pos).into_iter().flatten())
            .filter(|(p, _)| !used.contains(&p.id))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        if let Some((p, proj)) = flex {
            used.push(p.id.clone());
            out.push(("FLEX".into(), p.name.clone(), *proj));
        } else {
            out.push(("FLEX".into(), "(empty)".into(), 0.0));
        }
        take("K", "K", &mut used, &mut out, &by_pos);
        take("DST", "DEF", &mut used, &mut out, &by_pos);
        out
    }

    fn optimal_starter_ids(&self, roster: &[String]) -> Vec<String> {
        let names: Vec<String> = self.optimal_starters(roster).iter().map(|(_, n, _)| n.clone()).collect();
        names
            .iter()
            .filter_map(|n| self.players.iter().find(|p| &p.name == n).map(|p| p.id.clone()))
            .collect()
    }
}

fn generate_world() -> Sim {
    let mut players = Vec::new();
    let mut idc = 1000;
    let nfl = ["SF", "DAL", "KC", "BUF", "PHI", "MIA", "DET", "BAL", "CIN", "GB", "LAR", "NYJ"];
    let push = |pos: &'static str, count: usize, top: f64, floor: f64, players: &mut Vec<FakePlayer>, idc: &mut u32| {
        for i in 0..count {
            let skill = top - (top - floor) * (i as f64 / count as f64);
            players.push(FakePlayer {
                id: idc.to_string(),
                name: format!("{} {}{}", fake_first(*idc), pos_name(pos), i + 1),
                pos,
                nfl_team: nfl[i % nfl.len()].to_string(),
                skill,
            });
            *idc += 1;
        }
    };
    push("QB", 30, 26.0, 10.0, &mut players, &mut idc);
    push("RB", 60, 22.0, 3.0, &mut players, &mut idc);
    push("WR", 72, 21.0, 3.0, &mut players, &mut idc);
    push("TE", 36, 15.0, 2.0, &mut players, &mut idc);
    push("K", 20, 10.0, 5.0, &mut players, &mut idc);
    push("DEF", 20, 10.0, 3.0, &mut players, &mut idc);

    let teams = (1..=TEAMS)
        .map(|i| Team {
            roster_id: i,
            user_id: format!("U{i}"),
            display_name: format!("Owner{i}"),
            team_name: if i == 1 {
                Some("Bakers Gonna Sim".into())
            } else if i % 3 == 0 {
                Some(format!("Custom Squad {i}"))
            } else {
                None
            },
            players: Vec::new(),
            wins: 0,
            losses: 0,
            fpts: 0.0,
            fpts_against: 0.0,
        })
        .collect();

    Sim {
        week: 0,
        players,
        teams,
        draft_status: "drafting",
        draft_picks: Vec::new(),
        projections: HashMap::new(),
        matchup_hist: HashMap::new(),
        winners_bracket: Vec::new(),
    }
}

fn fake_first(seed: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "Alex", "Blake", "Casey", "Drew", "Emery", "Flynn", "Gray", "Harper", "Indy", "Jules",
        "Kai", "Lane",
    ];
    NAMES[(seed as usize) % NAMES.len()]
}

fn pos_name(pos: &str) -> &'static str {
    match pos {
        "QB" => "Slinger",
        "RB" => "Rusher",
        "WR" => "Catcher",
        "TE" => "Mismatch",
        "K" => "Legger",
        _ => "Wall",
    }
}

/// Snake slot for a given overall pick (1-indexed, no reversal round).
fn snake_roster_for_pick(pick_no: u32) -> u32 {
    let idx0 = (pick_no - 1) % TEAMS;
    let round0 = (pick_no - 1) / TEAMS;
    if round0 % 2 == 1 { TEAMS - idx0 } else { idx0 + 1 }
}

/// Needs-aware autodraft: everyone ends with 2QB/5RB/5WR/1TE/1K/1DEF worth of picks.
fn autodraft_pick(sim: &Sim, roster_id: u32) -> String {
    let team = &sim.teams[(roster_id - 1) as usize];
    let mut have: HashMap<&str, u32> = HashMap::new();
    for id in &team.players {
        *have.entry(sim.player(id).pos).or_insert(0) += 1;
    }
    let drafted: Vec<&String> = sim.draft_picks.iter().map(|(_, _, _, _, p)| p).collect();
    let round = team.players.len() as u32 + 1;
    let want = |pos: &str, have: &HashMap<&str, u32>| -> bool {
        let h = have.get(pos).copied().unwrap_or(0);
        match pos {
            "QB" => h < 2,
            "TE" => h < 2,
            "K" => h < 1 && round >= ROUNDS - 2,
            "DEF" => h < 1 && round >= ROUNDS - 2,
            _ => true,
        }
    };
    // Kicker/DEF are forced in the last two rounds if still missing.
    if round >= ROUNDS - 1 {
        for pos in ["K", "DEF"] {
            if have.get(pos).copied().unwrap_or(0) == 0 {
                if let Some(p) = sim
                    .players
                    .iter()
                    .filter(|p| p.pos == pos && !drafted.contains(&&p.id))
                    .max_by(|a, b| a.skill.partial_cmp(&b.skill).unwrap())
                {
                    return p.id.clone();
                }
            }
        }
    }
    sim.players
        .iter()
        .filter(|p| !drafted.contains(&&p.id) && want(p.pos, &have))
        .max_by(|a, b| a.skill.partial_cmp(&b.skill).unwrap())
        .map(|p| p.id.clone())
        .expect("player pool exhausted")
}

// ---------------------------------------------------------------------------
// Mock HTTP server (Sleeper + Anthropic)
// ---------------------------------------------------------------------------

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(hend) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..hend]).to_string();
            let cl: usize = head
                .lines()
                .find(|l| l.to_lowercase().starts_with("content-length:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < hend + 4 + cl {
                let n = sock.read(&mut tmp).await.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let body = String::from_utf8_lossy(&buf[hend + 4..hend + 4 + cl]).to_string();
            let path = head.lines().next()?.split_whitespace().nth(1)?.to_string();
            return Some((path, body));
        }
    }
}

fn league_json() -> Value {
    let mut positions: Vec<&str> = vec!["QB", "RB", "RB", "WR", "WR", "TE", "FLEX", "K", "DEF"];
    positions.extend(["BN"; 6]);
    json!({
        "league_id": "L1",
        "name": "Simulated Couples League",
        "season": "2026",
        "total_rosters": TEAMS,
        "status": "in_season",
        "roster_positions": positions,
        "scoring_settings": {"rec": 1.0},
        "settings": {},
        "draft_id": "D1"
    })
}

fn route(path: &str, body: &str, sim: &Arc<Mutex<Sim>>) -> String {
    let s = sim.lock();
    let path_no_q = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = path_no_q.trim_matches('/').split('/').collect();
    let v: Value = match parts.as_slice() {
        ["sleeper", "state", "nfl"] => {
            json!({"week": s.week.max(1), "season": "2026", "season_type": "regular", "display_week": s.week.max(1)})
        }
        ["sleeper", "user", "simuser"] | ["sleeper", "user", "U1"] => {
            json!({"user_id": "U1", "display_name": "simuser"})
        }
        ["sleeper", "user", "U1", "leagues", "nfl", "2026"] => json!([league_json()]),
        ["sleeper", "league", "L1"] => league_json(),
        ["sleeper", "league", "L1", "rosters"] => Value::Array(
            s.teams
                .iter()
                .map(|t| {
                    json!({
                        "roster_id": t.roster_id,
                        "owner_id": t.user_id,
                        "players": t.players,
                        "starters": if s.draft_status == "complete" { s.optimal_starter_ids(&t.players) } else { vec![] },
                        "settings": {
                            "wins": t.wins, "losses": t.losses, "ties": 0,
                            "fpts": t.fpts.trunc(), "fpts_decimal": (t.fpts.fract()*100.0).round(),
                            "fpts_against": t.fpts_against.trunc(),
                            "fpts_against_decimal": (t.fpts_against.fract()*100.0).round()
                        }
                    })
                })
                .collect(),
        ),
        ["sleeper", "league", "L1", "users"] => Value::Array(
            s.teams
                .iter()
                .map(|t| {
                    let mut u = json!({"user_id": t.user_id, "display_name": t.display_name});
                    if let Some(tn) = &t.team_name {
                        u["metadata"] = json!({"team_name": tn});
                    }
                    u
                })
                .collect(),
        ),
        ["sleeper", "league", "L1", "matchups", w] => {
            let week: u8 = w.parse().unwrap_or(1);
            match s.matchup_hist.get(&week) {
                Some(rows) => Value::Array(
                    rows.iter()
                        .map(|(mid, rid, pts, starters)| {
                            json!({"matchup_id": mid, "roster_id": rid, "points": pts, "starters": starters})
                        })
                        .collect(),
                ),
                None => {
                    // Future week: pairings known, no points yet.
                    Value::Array(
                        (1..=TEAMS)
                            .map(|rid| json!({"matchup_id": rid.div_ceil(2), "roster_id": rid, "points": 0.0}))
                            .collect(),
                    )
                }
            }
        }
        ["sleeper", "league", "L1", "transactions", _] => json!([
            {
                "transaction_id": "T1", "type": "waiver", "status": "complete",
                "roster_ids": [2], "adds": {s.players[150].id.clone(): 2}, "drops": {},
                "settings": {"waiver_bid": 17}, "leg": s.week.max(1)
            }
        ]),
        ["sleeper", "league", "L1", "traded_picks"] => json!([
            {"season": "2027", "round": 2, "roster_id": 3, "owner_id": 5, "previous_owner_id": 3}
        ]),
        ["sleeper", "league", "L1", "winners_bracket"] => Value::Array(s.winners_bracket.clone()),
        ["sleeper", "league", "L1", "losers_bracket"] => json!([]),
        ["sleeper", "league", "L1", "drafts"] => json!([
            {"draft_id": "D1", "status": s.draft_status, "settings": {"rounds": ROUNDS, "teams": TEAMS}}
        ]),
        ["sleeper", "draft", "D1"] => {
            let order: HashMap<String, u32> =
                s.teams.iter().map(|t| (t.user_id.clone(), t.roster_id)).collect();
            json!({"draft_id": "D1", "status": s.draft_status, "draft_order": order,
                   "settings": {"rounds": ROUNDS, "teams": TEAMS}})
        }
        ["sleeper", "draft", "D1", "picks"] => Value::Array(
            s.draft_picks
                .iter()
                .map(|(no, rd, rid, uid, pid)| {
                    json!({"pick_no": no, "round": rd, "roster_id": rid, "picked_by": uid, "player_id": pid})
                })
                .collect(),
        ),
        ["sleeper", "players", "nfl"] => {
            let map: serde_json::Map<String, Value> = s
                .players
                .iter()
                .map(|p| {
                    (
                        p.id.clone(),
                        json!({"player_id": p.id, "full_name": p.name, "position": p.pos, "team": p.nfl_team}),
                    )
                })
                .collect();
            Value::Object(map)
        }
        ["sleeper", "players", "nfl", "trending", dir] => {
            let drafted: Vec<&String> = s.draft_picks.iter().map(|(_, _, _, _, p)| p).collect();
            let mut fas: Vec<&FakePlayer> =
                s.players.iter().filter(|p| !drafted.contains(&&p.id)).collect();
            fas.sort_by(|a, b| b.skill.partial_cmp(&a.skill).unwrap());
            Value::Array(
                fas.iter()
                    .take(25)
                    .enumerate()
                    .map(|(i, p)| {
                        json!({"player_id": p.id, "count": if *dir == "add" { 5000 - i as u64 * 100 } else { 800 - i as u64 * 10 }})
                    })
                    .collect(),
            )
        }
        ["sleeper", "projections", "nfl", "regular", "2026", _] => {
            let map: serde_json::Map<String, Value> = s
                .projections
                .iter()
                .map(|(id, pts)| (id.clone(), json!({"pts_ppr": pts})))
                .collect();
            Value::Object(map)
        }
        ["claude", "v1", "messages"] => claude_response(body, &s),
        other => {
            panic!("mock server: unhandled route {:?}", other);
        }
    };
    v.to_string()
}

/// Canned Claude: inspects the prompt and answers in the exact formats the
/// parsers expect, generated from the simulated world.
fn claude_response(body: &str, s: &Sim) -> Value {
    let req: Value = serde_json::from_str(body).unwrap_or_default();
    let user_prompt = req["messages"][0]["content"].as_str().unwrap_or_default();
    let system = req["system"].as_str().unwrap_or_default();

    let text = if user_prompt.contains("Required starting slots") {
        // Lineup request → markdown-flavored response (exercises the tolerant parser).
        let mine = s.optimal_starters(&s.my_team().players);
        let mut out = String::from("**REASONING:** Simulated optimal lineup from mock Claude.\n**LINEUP:**\n");
        for (slot, name, _) in mine {
            out.push_str(&format!("- **{slot}:** {name}\n"));
        }
        out
    } else if system.contains("draft assistant") {
        let drafted: Vec<&String> = s.draft_picks.iter().map(|(_, _, _, _, p)| p).collect();
        let mut fas: Vec<&FakePlayer> =
            s.players.iter().filter(|p| !drafted.contains(&&p.id)).collect();
        fas.sort_by(|a, b| b.skill.partial_cmp(&a.skill).unwrap());
        fas.iter()
            .take(3)
            .enumerate()
            .map(|(i, p)| format!("{}. {} ({}) — best available at pick", i + 1, p.name, p.pos))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "Simulated analysis: package looks balanced; monitor injuries.".to_string()
    };

    json!({
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn"
    })
}

// ---------------------------------------------------------------------------
// The season
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn simulate_full_season() {
    // -- mock server ---------------------------------------------------------
    let sim = Arc::new(Mutex::new(generate_world()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    {
        let sim = sim.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let sim = sim.clone();
                tokio::spawn(async move {
                    if let Some((path, body)) = read_request(&mut sock).await {
                        let json = route(&path, &body, &sim);
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            json.len(),
                            json
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                    }
                });
            }
        });
    }

    let tmp = std::env::temp_dir().join(format!("sleeper-agent-sim-{}", std::process::id()));
    std::env::set_var("SLEEPER_API_BASE", format!("http://{addr}/sleeper"));
    std::env::set_var("ANTHROPIC_BASE_URL", format!("http://{addr}/claude"));
    std::env::set_var("SA_CACHE_DIR", &tmp);

    let anthropic = anthropic::Anthropic::new(AnthropicConfig {
        api_key: "sim-key".into(),
        backend: "api".into(),
        model: "claude-sim".into(),
        max_tokens: 2048,
        // Irrelevant on the `api` backend, which this sim uses.
        thinking_tokens: 0,
    })
    .unwrap();

    // -- connect -------------------------------------------------------------
    let client = Arc::new(SleeperClient::new().unwrap());
    let session = LeagueSession::connect(client, "simuser", None).await.unwrap();
    let settings = session.league_settings().await.unwrap();
    assert_eq!(settings.team_count, TEAMS);
    assert_eq!(settings.scoring, "ppr");
    println!("\n=== CONNECTED: Simulated Couples League ({} teams, {}) ===", TEAMS, settings.scoring);

    // -- phase 1: live draft -------------------------------------------------
    // 23 picks in; pick 24 is roster 1 (snake round 2) — that's us.
    {
        let mut s = sim.lock();
        for pick_no in 1..=23u32 {
            let rid = snake_roster_for_pick(pick_no);
            let uid = s.teams[(rid - 1) as usize].user_id.clone();
            let pid = autodraft_pick(&s, rid);
            s.teams[(rid - 1) as usize].players.push(pid.clone());
            s.draft_picks.push((pick_no, (pick_no - 1) / TEAMS + 1, rid, uid, pid));
        }
    }
    let dm = draft::DraftManager {
        session: &session,
        anthropic: &anthropic,
        strategy: Strategy::Balanced,
        my_team_name: "Bakers Gonna Sim".into(),
    };
    let state = dm.snapshot().await.unwrap();
    assert_eq!(state.current_pick, 24);
    assert!(dm.is_my_turn(&state), "pick 24 must be ours (snake round 2, custom team name)");
    let sugg = dm.ask_claude(&state, &[]).await.unwrap();
    assert_eq!(sugg.picks.len(), 3, "3 AI draft suggestions expected");
    println!("\n=== DRAFT: on the clock at pick 24 — AI suggests ===");
    for p in &sugg.picks {
        println!("  {}. {} ({:?}) — {}", p.rank, p.name, p.position, p.rationale);
    }

    // Auto-complete the rest of the draft.
    {
        let mut s = sim.lock();
        for pick_no in 24..=(TEAMS * ROUNDS) {
            let rid = snake_roster_for_pick(pick_no);
            let uid = s.teams[(rid - 1) as usize].user_id.clone();
            let pid = autodraft_pick(&s, rid);
            s.teams[(rid - 1) as usize].players.push(pid.clone());
            s.draft_picks.push((pick_no, (pick_no - 1) / TEAMS + 1, rid, uid, pid));
        }
        s.draft_status = "complete";
        s.week = 1;
        for t in &s.teams {
            assert_eq!(t.players.len(), ROUNDS as usize, "every team drafted {ROUNDS}");
        }
    }
    println!("\n=== DRAFT COMPLETE: {} picks ===", TEAMS * ROUNDS);

    // -- phase 2: regular season --------------------------------------------
    let mut rng = Lcg(0xC0FFEE);
    for week in 1..=REG_SEASON_WEEKS {
        {
            let mut s = sim.lock();
            s.week = week;
            let projections: HashMap<String, f64> = s
                .players
                .iter()
                .map(|p| (p.id.clone(), (p.skill * rng.range(0.7, 1.3)).max(0.0)))
                .collect();
            s.projections = projections;
        }
        session.invalidate_team_cache();

        let roster = session.my_roster(week).await.unwrap();
        assert_eq!(roster.players.len(), ROUNDS as usize);
        let l = lineup::ai_optimize(&anthropic, &roster, &settings, &[], &[], Strategy::Balanced, week)
            .await
            .unwrap();
        assert!(
            l.reasoning.contains("Simulated optimal lineup"),
            "week {week}: AI lineup must parse (got: {})",
            l.reasoning
        );
        assert_eq!(l.starters.len(), 9, "week {week}: 9 starters");
        assert!(
            l.starters.iter().all(|slot| slot.player.is_some()),
            "week {week}: every slot filled"
        );

        // Play the week: every team fields its optimal lineup; actual = proj ± noise.
        {
            let mut s = sim.lock();
            let mut scores: Vec<(u32, f32, Vec<String>)> = Vec::new();
            for t in &s.teams {
                let starters = s.optimal_starter_ids(&t.players);
                let proj: f64 = starters
                    .iter()
                    .map(|id| s.projections.get(id).copied().unwrap_or(0.0))
                    .sum();
                let actual = (proj * rng.range(0.8, 1.2)) as f32;
                scores.push((t.roster_id, actual, starters));
            }
            let mut rows = Vec::new();
            for m in 0..(TEAMS / 2) {
                let (a, b) = (2 * m as usize, 2 * m as usize + 1);
                let (rid_a, pts_a, st_a) = scores[a].clone();
                let (rid_b, pts_b, st_b) = scores[b].clone();
                let (wa, wb) = if pts_a >= pts_b { (1, 0) } else { (0, 1) };
                {
                    let ta = &mut s.teams[rid_a as usize - 1];
                    ta.wins += wa;
                    ta.losses += wb;
                    ta.fpts += pts_a as f64;
                    ta.fpts_against += pts_b as f64;
                }
                {
                    let tb = &mut s.teams[rid_b as usize - 1];
                    tb.wins += wb;
                    tb.losses += wa;
                    tb.fpts += pts_b as f64;
                    tb.fpts_against += pts_a as f64;
                }
                rows.push((m + 1, rid_a, pts_a, st_a));
                rows.push((m + 1, rid_b, pts_b, st_b));
            }
            s.matchup_hist.insert(week, rows);
        }
        let me = sim.lock().my_team().clone();
        println!(
            "  week {week:>2}: lineup proj {:>6.1} | record {}-{} | PF {:>7.1}",
            l.projected_total, me.wins, me.losses, me.fpts
        );

        // Mid-season checks.
        if week == 5 {
            let report = waiver::analyze(&session, &anthropic, Strategy::Balanced, &[], 25)
                .await
                .unwrap();
            assert!(!report.candidates.is_empty(), "waiver candidates expected");
            assert!(!report.raw.contains("unavailable"), "waiver AI should answer");
            println!(
                "    ↳ waiver check: {} candidates, top: {} (score noted: {})",
                report.candidates.len(),
                report.candidates[0].player.name,
                report.candidates[0].reasoning
            );
        }
        if week == 8 {
            let all = session.all_rosters(week).await.unwrap();
            let my = session.my_roster(week).await.unwrap();
            let partner = all.iter().find(|r| r.team_id != my.team_id).unwrap();
            let a = trade::analyze(
                &anthropic,
                &my,
                &partner.team_name,
                &[my.players[1].name.clone()],
                &[partner.players[1].name.clone()],
                &all,
                Strategy::Balanced,
                &[],
            )
            .await
            .unwrap();
            assert!(["ACCEPT", "DECLINE", "NEGOTIATE"].contains(&a.verdict));
            assert!(!a.ai_summary.is_empty(), "trade AI summary expected");
            println!(
                "    ↳ trade check vs {}: send {} / recv {} → {} (net {:+.1})",
                partner.team_name, my.players[1].name, partner.players[1].name, a.verdict, a.net_ros_delta
            );
        }
    }

    // -- phase 3: playoffs ---------------------------------------------------
    let seeds: Vec<Team> = {
        let s = sim.lock();
        let mut ts = s.teams.clone();
        ts.sort_by(|a, b| b.wins.cmp(&a.wins).then(b.fpts.partial_cmp(&a.fpts).unwrap()));
        ts.into_iter().take(4).collect()
    };
    {
        let mut s = sim.lock();
        s.week = 15;
        s.winners_bracket = vec![
            json!({"r": 1, "m": 1, "t1": seeds[0].roster_id, "t2": seeds[3].roster_id}),
            json!({"r": 1, "m": 2, "t1": seeds[1].roster_id, "t2": seeds[2].roster_id}),
            json!({"r": 2, "m": 3, "t1": {"w": 1}, "t2": {"w": 2}}),
        ];
    }
    session.invalidate_team_cache();
    let (winners, _) = session.playoff_bracket().await.unwrap();
    assert_eq!(winners.len(), 3);
    let final_match = winners.iter().find(|m| m.round == 2).unwrap();
    assert_eq!(final_match.team1.as_deref(), Some("Winner of M1"));
    println!("\n=== PLAYOFFS ===");
    for m in &winners {
        println!(
            "  R{} M{}: {} vs {}",
            m.round,
            m.match_id,
            m.team1.as_deref().unwrap_or("?"),
            m.team2.as_deref().unwrap_or("?")
        );
    }

    // Play the bracket: higher season fpts wins each matchup.
    let semi1 = if seeds[0].fpts >= seeds[3].fpts { &seeds[0] } else { &seeds[3] };
    let semi2 = if seeds[1].fpts >= seeds[2].fpts { &seeds[1] } else { &seeds[2] };
    let champ = if semi1.fpts >= semi2.fpts { semi1 } else { semi2 };
    {
        let mut s = sim.lock();
        s.week = 17;
        s.winners_bracket = vec![
            json!({"r": 1, "m": 1, "t1": seeds[0].roster_id, "t2": seeds[3].roster_id, "w": semi1.roster_id}),
            json!({"r": 1, "m": 2, "t1": seeds[1].roster_id, "t2": seeds[2].roster_id, "w": semi2.roster_id}),
            json!({"r": 2, "m": 3, "t1": semi1.roster_id, "t2": semi2.roster_id, "w": champ.roster_id, "p": 1}),
        ];
    }
    let (winners, _) = session.playoff_bracket().await.unwrap();
    let title = winners.iter().find(|m| m.placing == Some(1)).unwrap();
    assert!(title.winner.is_some(), "champion must resolve to a team name");

    // -- final report --------------------------------------------------------
    println!("\n=== FINAL STANDINGS ===");
    {
        let s = sim.lock();
        let mut ts = s.teams.clone();
        ts.sort_by(|a, b| b.wins.cmp(&a.wins).then(b.fpts.partial_cmp(&a.fpts).unwrap()));
        for (i, t) in ts.iter().enumerate() {
            let name = t.team_name.clone().unwrap_or_else(|| t.display_name.clone());
            println!(
                "  {:>2}. {:<20} {:>2}-{:<2}  PF {:>7.1}  PA {:>7.1}{}",
                i + 1,
                name,
                t.wins,
                t.losses,
                t.fpts,
                t.fpts_against,
                if t.roster_id == 1 { "   ← us" } else { "" }
            );
        }
    }
    println!("\n🏆 CHAMPION: {}\n", title.winner.as_deref().unwrap());

    let _ = std::fs::remove_dir_all(&tmp);
}
