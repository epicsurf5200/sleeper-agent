//! Live check that trade suggestions come back as parseable JSON.
//! Hits the real Sleeper API and a real Claude backend, so it is #[ignore]d:
//!   cargo test --test live_trade -- --ignored --nocapture
use sleeper_agent::{anthropic::Anthropic, api::{LeagueSession, SleeperClient}, config::Config, trade};
use std::sync::Arc;

#[tokio::test]
#[ignore]
async fn suggest_ideas_parses_into_structured_trades() {
    let cfg = Config::load(Config::default_path()).expect("config");
    let client = Arc::new(SleeperClient::new().unwrap());
    let session = LeagueSession::connect(
        client,
        &cfg.sleeper.username,
        Some(cfg.sleeper.league_id.as_str()).filter(|s| !s.is_empty()),
    )
    .await
    .expect("connect");

    let anthropic = Anthropic::new(cfg.anthropic.clone())
        .unwrap()
        .with_context(cfg.load_context().unwrap());

    let week = session.current_week().await.unwrap();
    let me = session.my_roster(week).await.unwrap();
    let all = session.all_rosters(week).await.unwrap();

    for (label, opts) in [
        (
            "single-step, rest of season",
            trade::SuggestOptions {
                count: 2,
                multi_tier: false,
                horizon: trade::Horizon::RestOfSeason,
                week,
                ..Default::default()
            },
        ),
        (
            "multi-tier, this week",
            trade::SuggestOptions {
                count: 2,
                multi_tier: true,
                horizon: trade::Horizon::ThisWeek,
                week,
                ..Default::default()
            },
        ),
    ] {
        let (ideas, raw) =
            trade::suggest_ideas(&anthropic, &me, &all, cfg.settings.strategy, &[], &opts)
                .await
                .expect("suggest");
        println!("\n--- {label}: {} idea(s) ---", ideas.len());
        assert!(
            !ideas.is_empty(),
            "no ideas parsed from reply:\n{}",
            &raw[..raw.len().min(600)]
        );
        for i in &ideas {
            println!("  {} ({} step(s))", i.headline, i.steps.len());
            for s in &i.steps {
                println!("    with {}: {:?} -> {:?}", s.partner, s.send, s.receive);
            }
            // Every named player must exist, or the card renders a warning.
            let names: Vec<&String> =
                i.steps.iter().flat_map(|s| s.send.iter().chain(s.receive.iter())).collect();
            let unknown: Vec<&&String> = names
                .iter()
                .filter(|n| {
                    !all.iter()
                        .flat_map(|r| r.players.iter())
                        .any(|p| p.name.eq_ignore_ascii_case(n))
                })
                .collect();
            if !unknown.is_empty() {
                println!("    !! names not on any roster: {unknown:?}");
            }
        }
    }
}
