//! Live smoke test for the projection-accuracy table. Hits the real Sleeper
//! API, so it is #[ignore]d by default: `cargo test --test live_perf -- --ignored`
use sleeper_agent::{api::SleeperClient, player_detail};

#[tokio::test]
#[ignore]
async fn builds_accuracy_table_from_live_api() {
    let c = SleeperClient::new().unwrap();
    let state = c.state().await.unwrap();
    let (season, weeks) =
        player_detail::scoring_season(&c, &state.season, &state.previous_season)
            .await
            .unwrap();
    println!("season={season} completed_weeks={}", weeks.len());

    let t0 = std::time::Instant::now();
    let table = player_detail::build_perf_table(&c, &season, &weeks, "pts_ppr")
        .await
        .unwrap();
    println!(
        "built in {:?}: {} players over {} weeks",
        t0.elapsed(),
        table.records.len(),
        table.weeks
    );
    assert!(!table.is_empty(), "expected a non-empty accuracy table");

    let mut sample: Vec<_> = table
        .records
        .iter()
        .filter(|(_, r)| r.games >= 10)
        .collect();
    sample.sort_by(|a, b| b.1.beat_pct().partial_cmp(&a.1.beat_pct()).unwrap());
    for (pid, r) in sample.iter().take(5) {
        println!(
            "  {pid}: {}/{} beat ({:.0}%), avg {:+.1} vs proj {:.1}",
            r.beat, r.games, r.beat_pct(), r.avg_diff(), r.avg_proj()
        );
    }

    // Second call must come from the disk cache and be effectively instant.
    let t1 = std::time::Instant::now();
    let again = player_detail::build_perf_table(&c, &season, &weeks, "pts_ppr")
        .await
        .unwrap();
    println!("cached rebuild in {:?}", t1.elapsed());
    assert_eq!(again.records.len(), table.records.len());
}
