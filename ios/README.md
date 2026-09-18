# Fantasy Agent (iPhone)

A native SwiftUI app sharing the desktop app's Rust core. Same data, same
analysis, same palette — a phone-shaped front end rather than a second
implementation.

The phone app ships as **Fantasy Agent** (`dev.fantasy-agent.ios`) rather than
Sleeper Agent. Apple review takes a dim view of a third-party app leading with
another company's product name, and the App Store is the one place that
judgement is enforced. The desktop app keeps its original name.

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

Signing is detected automatically: the Team ID comes from `$DEVELOPMENT_TEAM`
or the OU field of an Apple Development certificate in your keychain, and lands
in a gitignored `Local.xcconfig`. A Team ID identifies a developer account, so
it is not committed.

Then in Xcode: select your iPhone, set **Signing & Capabilities → Team**, Run.

Flags:

| Flag      | Effect                                                         |
| --------- | -------------------------------------------------------------- |
| `--debug` | Debug profile — much faster to compile while iterating          |
| `--sim`   | Also build the simulator slice                                  |
| `--open`  | Open the generated project when finished                        |
| `--testflight` | Archive Release and upload to TestFlight                    |

The first release build of the core takes several minutes; it compiles the
whole dependency tree for `aarch64-apple-ios`.

## TestFlight

Either credential works:

```sh
# App Store Connect API key — preferred
ASC_KEY_ID=XXXXXXXXXX ASC_ISSUER_ID=<uuid> ./ios/build.sh --testflight

# or an Apple ID with an app-specific password
APPLE_ID=you@example.com APP_SPECIFIC_PASSWORD=xxxx-xxxx-xxxx-xxxx \
  ./ios/build.sh --testflight
```

The API key pair comes from **App Store Connect → Users and Access →
Integrations → App Store Connect API**; the `.p8` is read from
`~/.appstoreconnect/private_keys/AuthKey_<KEY_ID>.p8` (override with
`ASC_KEY_PATH`). App-specific passwords are created at
[appleid.apple.com](https://appleid.apple.com) → Sign-In and Security. Nothing
secret is written to the repo, and the password is passed to `altool` through
the environment rather than argv, where other processes could read it.

The API key is preferred because `-allowProvisioningUpdates` can then create
the distribution certificate and App Store profile for you. An *Apple
Development* certificate does not cover App Store distribution, so on the
Apple ID route that certificate has to already exist — add the account under
**Xcode → Settings → Accounts** and let Xcode manage it.

The build number is set from the repo's commit count, since App Store Connect
rejects a number it has already accepted. Override with `BUILD_NUMBER=…` if you
need to.

### First-time setup

`ios/asc.py` is a small dependency-free client for the same credentials
(it signs the ES256 JWT with `openssl`, since neither PyJWT nor
`cryptography` ships with macOS Python):

```sh
export ASC_KEY_ID=XXXXXXXXXX ASC_ISSUER_ID=<uuid>
./ios/asc.py whoami                        # check the credentials work
./ios/asc.py register-bundle dev.fantasy-agent.ios "Fantasy Agent"
./ios/asc.py apps                          # list what already exists
```

The **app record itself must be created in the web UI** — the App Store
Connect API exposes `apps` as read/update only, with no create. Go to
**App Store Connect → Apps → + → New App**, pick iOS, select the
`dev.fantasy-agent.ios` bundle ID, and choose a name and SKU. Upload is
rejected until that record exists.

On a **free Apple ID** there is no TestFlight, and sideloaded builds stop
launching after 7 days. The Apple Developer Program removes both limits.

## Layout

```
ios/
├── sa-ffi/           # Rust: C ABI over the shared core
│   └── src/lib.rs    # one JSON request/response entry point
├── FantasyAgent/
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
