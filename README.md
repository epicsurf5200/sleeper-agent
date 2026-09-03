<p align="center">
  <img src="assets/logo-lockup.svg" alt="SLEEPER AGENT" width="320">
</p>

# sleeper-agent

> *An agent for Sleeper. A sleeper agent for your league.*

An autonomous, **Claude-powered** fantasy football manager built natively on
the [Sleeper API](https://docs.sleeper.com/). It watches your league in the
background, sets lineups by strategy, hunts the waiver wire with real
projections **and** league-wide trending data, evaluates trades (players *and*
picks), and runs your draft with on-the-clock detection.

Forked from [groks_fantasy](https://github.com/epicsurf5200/groks_fantasy)
and rebuilt around everything Sleeper's API offers.

## Why Sleeper-native?

The multi-provider ancestor treated every platform as the lowest common
denominator. Going all-in on Sleeper unlocks:

| Sleeper API feature                | What sleeper-agent does with it                                   |
| ---------------------------------- | ----------------------------------------------------------------- |
| No-auth public API                 | Zero cookies/OAuth — just your username                           |
| `GET /user/…/leagues`              | **Auto-discovers your leagues** (`sa leagues` to list/pin)        |
| `GET /projections/nfl/…`           | **Real weekly projections**, matched to your league's scoring     |
| `GET /players/nfl/trending/add`    | Waiver candidates boosted by **league-wide add counts (24h)**     |
| `GET /league/…/transactions/…`     | Recent waivers/trades/FA moves (incl. **FAAB bids**) fed to Claude |
| `GET /league/…/traded_picks`       | Future picks in trade context                                     |
| `GET /league/…/winners_bracket`    | Playoff bracket view                                              |
| `GET /draft/…` + `draft_order`     | Live draft feed with **snake-order on-the-clock detection**       |
| `GET /players/nfl` (5 MB)          | Full player DB, **disk-cached 24 h** per Sleeper's guidance       |
| `GET /stats/nfl/…`                 | Weekly actuals (exposed on the client for future use)             |

## Install

```sh
# Rust 1.78+
git clone https://github.com/epicsurf5200/sleeper-agent.git
cd sleeper-agent
cargo build --release          # binaries: target/release/sa, sa-gui
```

Headless (no GUI deps): `cargo build --release --bin sa --no-default-features`

## Configure

```sh
./target/release/sa init       # writes config.yaml
$EDITOR config.yaml            # set sleeper.username — that's it
export ANTHROPIC_API_KEY=sk-ant-...
./target/release/sa leagues    # list your leagues, pin one if desired
```

Only your **Sleeper username** is required. If you're in multiple leagues,
pin one via `sleeper.league_id` or `--league <id>`; otherwise the first
discovered league is used.

## Run

```sh
sa                       # terminal UI (default)
sa gui                   # desktop GUI (or: sa-gui)
sa info                  # league summary
sa roster                # roster with real projections
sa lineup [--week N]     # AI lineup
sa waiver                # waiver report: projections + trending + FAAB advice
sa trade -p "Team B" -s "Player A, Player B" -r "Player C"
sa trending              # league-wide adds/drops, last 24h
sa transactions -w 3     # league activity, last 3 weeks
sa bracket               # playoff brackets
sa traded-picks          # future picks that changed hands
sa draft -i 5            # watch draft; AI suggestions when you're on the clock
sa draft-suggest         # one-shot pick suggestion
sa daemon                # headless monitor: analyse on a timer, alert on changes
sa -s high_stakes lineup # strategy override on any command
```

## iPhone app

A native SwiftUI app shares this crate as its core — same API layer, same
analysis, same palette. It ships as **Fantasy Agent**, since Apple review is
unfriendly to a third-party app fronting another company's product name:

```sh
./ios/build.sh --open      # build the Rust core, generate the Xcode project
```

Everything crosses into Swift as JSON through a four-symbol C ABI
(`ios/sa-ffi`), so adding a feature means adding a request variant rather than
a new exported symbol. See [ios/README.md](ios/README.md).

One platform limit worth knowing up front: iOS forbids spawning subprocesses,
so the `claude-cli` backend cannot run there and the phone needs an Anthropic
API key. Everything non-AI works without one.

## Background monitoring

`sa daemon` runs the same analysis on a schedule and pushes an alert to a
webhook (Discord by default) when the recommendation actually changes — a
better lineup is available, a starter is ruled out, a waiver upgrade appears,
or Claude finds a trade worth proposing. Alerts are deduped by content
fingerprint, so a 3-hour interval doesn't re-send the same advice all day.

```sh
sa daemon --once --dry-run   # print what it would send, send nothing
sa daemon --once             # one real cycle
sa daemon                    # loop forever (what the systemd unit runs)
```

Configure it under `notify:` and `daemon:` in config.yaml, or in the GUI's
**Settings** tab. To leave it running on a Proxmox LXC or any Linux box, see
[deploy/README.md](deploy/README.md) — systemd units, an installer, and the
webhook/secret setup.

### Strategies

- `conservative` — floor over ceiling, avoid injury tags.
- `balanced` — risk-adjusted expected value (default).
- `high_stakes` — chase ceiling, tolerate risk, stack upside.

Each strategy adjusts both the local metric model (injury multipliers,
variance weighting, strategy-fit score) and the guidance given to Claude.

### TUI tabs

Roster · Lineup · Waiver · Trade · **Trending** · **Activity** (transactions +
traded picks) · **Bracket** · Draft · News · Help — keys: `r` refresh, `l`
lineup, `w` waiver, `g` trending, `a` activity, `b` bracket, `d` draft,
`s` strategy, `1‑9/0` jump, `q` quit.

## How the waiver score works

```
score = (candidate ROS − weakest same-position rostered ROS)
        × (0.5 + 0.5 × strategy_fit)
        × (1 + ln(1 + trending_adds) / 10)
```

ROS values come from Sleeper's weekly projections (scoring-matched:
`pts_ppr` / `pts_half_ppr` / `pts_std`), risk-adjusted by injury status per
strategy. The top 8 shortlist goes to Claude along with your league's recent
transactions and roster-filtered news; Claude returns a final top 5 with
FAAB bid sizing advice.

## Architecture

```
src/
├── api.rs         # SleeperClient (every endpoint) + LeagueSession (domain layer)
├── main.rs        # `sa` CLI
├── ui.rs          # ratatui TUI (10 tabs)
├── gui.rs         # egui desktop GUI (9 tabs, incl. Settings) [feature "gui"]
├── daemon.rs      # headless monitor loop: triggers → deduped alerts
├── notify.rs      # Discord / raw-JSON webhook delivery
├── bin/gui_main.rs# `sa-gui`
├── anthropic.rs   # Claude Messages API client
├── strategy.rs    # conservative / balanced / high_stakes
├── metrics.rs     # PlayerMetrics + PackageMetrics
├── lineup.rs      # greedy baseline + Claude-refined optimizer
├── waiver.rs      # projections × trending × transactions → Claude re-rank
├── trade.rs       # package metrics + verdict + Claude summary
├── draft.rs       # live draft manager with on-the-clock detection
├── scheduler.rs   # background refresh loop (one AppData snapshot for all UIs)
├── news.rs        # RSS ingest, roster-filtered
├── types.rs       # domain types incl. Transaction/TradedPick/Bracket/Trending
└── config.rs      # YAML config (username + optional league_id)

ios/
├── sa-ffi/        # C ABI bridge over the same core
└── FantasyAgent/ # SwiftUI app (Core/, Views/, Theme.swift)
```

## Notes

- The player DB (~5 MB) is cached at `~/.cache/sleeper-agent/players_nfl.json`
  and refreshed at most daily, per Sleeper's API guidance.
- Sleeper's API is read-only — lineup changes are recommendations you apply
  in the Sleeper app.
- The projections/stats endpoints are widely used but not formally documented;
  if they change shape the agent degrades gracefully (falls back to metric
  heuristics).

## License

MIT
