/// Build provenance, stamped by build.rs. Surfaced in `sa --version` and the
/// GUI's Settings tab so a stale binary is identifiable at a glance.
pub mod build_info {
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
    pub const COMMIT: &str = env!("SA_GIT_COMMIT");
    pub const COMMIT_DATE: &str = env!("SA_GIT_DATE");

    /// e.g. "0.1.0 (9ac00c2, 2026-08-31)". A const rather than a function so
    /// clap can take it directly as a `&'static str`.
    pub const LONG_VERSION: &str = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("SA_GIT_COMMIT"),
        ", ",
        env!("SA_GIT_DATE"),
        ")"
    );
}

pub mod anthropic;
pub mod api;
pub mod backtest;
pub mod config;
pub mod draft;
#[cfg(feature = "gui")]
pub mod gui;
#[cfg(feature = "gui")]
pub mod images;
pub mod lineup;
pub mod metrics;
pub mod daemon;
pub mod news;
pub mod player_detail;
pub mod notify;
pub mod scheduler;
pub mod strategy;
pub mod trade;
pub mod types;
pub mod ui;
pub mod waiver;
