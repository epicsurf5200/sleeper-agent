# Sleeper Agent for iPhone

A native SwiftUI app sharing the desktop app's Rust core. Same data, same
analysis, same palette — a phone-shaped front end rather than a second
implementation.

## Read this first: no subscription auth on iOS

The desktop app can talk to Claude two ways: an API key, or the `claude` CLI
using your Pro/Max subscription. **The CLI route cannot exist on iPhone.** iOS
forbids an app from spawning subprocesses, so there is no way to run the Claude
Code binary inside the sandbox — this is an OS rule, not a gap in the port.

The app therefore requires an **Anthropic API key**, billed per token, entered
under Settings → Claude. Everything that is not AI-backed (rosters, matchups,
projections, trending, news, the league comparison and its radar) works with no
key at all, because that data comes from Sleeper.

`Config::for_ios` pins the backend to `api` rather than leaving it on `auto`,
so a missing key produces a clear message instead of silently resolving to a
CLI that can never run.

## Build

```sh
./ios/build.sh --open      # build the core, generate the project, open Xcode
```

Then in Xcode: select your iPhone, set **Signing & Capabilities → Team**, Run.

Flags:

| Flag      | Effect                                                         |
| --------- | -------------------------------------------------------------- |
| `--debug` | Debug profile — much faster to compile while iterating          |
| `--sim`   | Also build the simulator slice                                  |
| `--open`  | Open the generated project when finished                        |

The first release build of the core takes several minutes; it compiles the
whole dependency tree for `aarch64-apple-ios`.

## Signing, and the 7-day thing

A **free Apple ID** works, but the provisioning profile expires after 7 days
and the app stops launching until you rebuild and reinstall. That is Apple's
limit on free accounts, not something the project can work around.

The **Apple Developer Program** ($99/year) gives year-long profiles and
TestFlight. Worth it only if you get tired of the weekly reinstall.

## Layout

```
ios/
├── sa-ffi/           # Rust: C ABI over the shared core
│   └── src/lib.rs    # one JSON request/response entry point
├── SleeperAgent/
│   ├── Core/         # SACore (FFI wrapper), Models, AppState
│   ├── Views/        # one file per screen
│   ├── Theme.swift   # palette shared with the desktop app
│   └── sa_ffi.h      # C header, mirrors sa-ffi/src/lib.rs
├── project.yml       # XcodeGen spec — the .xcodeproj is generated
└── build.sh
```

The `.xcodeproj` is generated and gitignored on purpose: a `.pbxproj` is
merge-hostile and effectively unreviewable, whereas `project.yml` is both.
Regenerate any time with `./ios/build.sh`.

## How the bridge works

Everything crosses as JSON through a single `sa_request` call:

```c
void sa_request(SAEngine *engine, const char *request_json,
                void *ctx, SAResponseCallback cb);
```

Four exported symbols total, one of which is a version string. Adding a feature
means adding a `Request` variant in Rust and a call site in `AppState` — never
a new C symbol, a new header entry, and a new Swift shim.

Requests are a serde-tagged enum (`{"op":"lineup"}`), replies are
`{"ok":true,"data":…}` or `{"ok":false,"error":…}`. `SACore` parks each
in-flight continuation in a retained box, hands it over as the opaque `ctx`,
and reclaims it in the callback — the C callback cannot capture Swift context,
so this is what makes `await` work.

The reply string is freed as soon as the callback returns, so Swift copies it
immediately. Every call runs on the Rust runtime's worker threads, off the UI
thread.

## What differs from the desktop app

- **Player detail is a sheet, not a side panel.** There is no room to split a
  phone screen; the content is the same.
- **Nine screens, five tabs.** iOS shows at most five, so Waiver, Trending,
  League, News and Settings live behind **More**.
- **No background daemon.** Scheduled alerts belong to the headless
  deployment — see [../deploy/README.md](../deploy/README.md). iOS suspends
  apps aggressively, so a phone is the wrong place to run a monitor loop.
- **No `context_files`.** Those are desktop paths with no phone equivalent.

## Storage

| What                     | Where                                        |
| ------------------------ | -------------------------------------------- |
| `config.yaml`            | `Application Support/sleeper-agent/`          |
| Player DB, accuracy table | `Caches/sleeper-agent/`                      |

Config lives in Application Support because it is not re-downloadable; the
5 MB player database and the projection-accuracy table live in Caches, where
iOS may reclaim them under storage pressure and the app will simply refetch.

Headshots are fetched by SwiftUI's `AsyncImage` straight from Sleeper's CDN
rather than through the Rust image cache — the URL is deterministic per player
id, and `AsyncImage` already handles the caching.
