# Headless deployment (Proxmox / any Linux box)

`sa daemon` runs the same analysis the GUI does, on a timer, and pushes an
alert to a webhook when something changes. It is the piece you leave running
on a server.

## What it alerts on

Each trigger can be turned off individually in `daemon.triggers`:

| Trigger            | Fires when                                                         |
| ------------------ | ------------------------------------------------------------------ |
| `better_lineup`    | The AI lineup differs from the lineup you currently have set in Sleeper |
| `injured_starter`  | A player you are starting is Out / Doubtful / IR / Suspended        |
| `waiver`           | A free agent would upgrade a weak roster spot                       |
| `trade`            | Claude finds a concrete, plausible trade against another roster     |

Alerts are deduped by content fingerprint, persisted to
`$SA_CACHE_DIR/daemon-state.json`. A 3-hour interval therefore does **not**
re-send the same "start Player X" alert eight times a day — only when the
recommendation actually changes.

## Quick start on a Proxmox LXC

Create an unprivileged Debian 12 container (1 vCPU / 512 MB / 4 GB disk is
plenty — the only sizeable thing on disk is Sleeper's ~5 MB player DB), then:

```sh
apt update && apt install -y git curl build-essential pkg-config libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

git clone https://github.com/epicsurf5200/sleeper-agent.git
cd sleeper-agent
sudo ./deploy/install.sh
```

The installer builds `sa` headless (`--no-default-features`, so no GUI
toolkit is pulled in), creates a `sleeper` system user, and installs the
units. It then prints the three things you still have to fill in.

Building in a 512 MB container can OOM — if `cargo build` gets killed, either
bump the container to 2 GB for the build, or build on your workstation
(`cargo build --release --bin sa --no-default-features --target
x86_64-unknown-linux-gnu`) and drop the binary at `target/release/sa` in the
checkout before running the installer, which will then skip the build.

## Configure

`/etc/sleeper-agent/config.yaml`:

```yaml
sleeper:
  username: "your-sleeper-name"

notify:
  webhook_url: ""          # leave empty — comes from SA_WEBHOOK_URL
  format: discord          # discord | json

daemon:
  interval_minutes: 180
  active_hour_start: 8     # local time; no alerts outside this window
  active_hour_end: 23
  triggers:
    better_lineup: true
    injured_starter: true
    waiver: true
    trade: true
```

`/etc/sleeper-agent/sleeper-agent.env` holds the secrets
(`ANTHROPIC_API_KEY`, `SA_WEBHOOK_URL`) and `TZ`. Quiet hours are evaluated
in **local** time, so set `TZ` or `timedatectl set-timezone` — a UTC container
will otherwise think 3am local is a fine time to analyse.

### Discord webhook

Server Settings → Integrations → Webhooks → New Webhook, pick a channel, Copy
Webhook URL, paste it into `SA_WEBHOOK_URL`. Verify before enabling the
service:

```sh
sudo -u sleeper SA_CONFIG=/etc/sleeper-agent/config.yaml \
  SA_CACHE_DIR=/var/cache/sleeper-agent \
  SA_WEBHOOK_URL='https://discord.com/api/webhooks/...' \
  /opt/sleeper-agent/sa daemon --once
```

For Home Assistant, n8n, ntfy or anything else, set `format: json` and the
raw alert (`kind`, `title`, `body`, `fingerprint`) is POSTed instead of a
Discord embed.

## Two scheduling models

**Long-running loop** (default) — the process stays up and sleeps
`interval_minutes` between cycles, honouring `active_hour_*`:

```sh
systemctl enable --now sleeper-agent
```

**systemd timer** — the process exits between runs; systemd schedules it:

```sh
systemctl enable --now sleeper-agent.timer
systemctl list-timers sleeper-agent.timer
```

With the timer, `OnCalendar` in `sleeper-agent.timer` is the schedule —
`interval_minutes` and `active_hour_*` are ignored, because `--once` treats an
explicit single cycle as "the operator asked for this now". Use one model or
the other, not both.

## Claude access on a headless box

**API key (recommended).** Set `ANTHROPIC_API_KEY` in the env file and leave
`anthropic.backend: auto`. Nothing interactive, nothing to re-auth.

**Claude Pro/Max subscription.** Set `anthropic.backend: claude-cli` and
install the Claude Code CLI as the service user. This needs an interactive
login once (`claude` writes credentials under the user's home) and three
changes to `sleeper-agent.service`, because the hardening in the shipped unit
blocks Node:

```ini
ProtectHome=false            # claude reads ~/.claude credentials
MemoryDenyWriteExecute=false # V8 JIT needs W^X-violating pages
ReadWritePaths=/var/cache/sleeper-agent /home/sleeper/.claude
```

Give the service user a real home and shell for the login step
(`usermod -d /home/sleeper -s /bin/bash sleeper`). The API key path avoids all
of this; prefer it unless you specifically want subscription billing.

## Operating it

```sh
journalctl -u sleeper-agent -f           # follow
systemctl status sleeper-agent
sa daemon --once --dry-run               # print what it *would* send, send nothing
rm /var/cache/sleeper-agent/daemon-state.json   # forget dedupe, re-alert everything
```

`--dry-run` is the right way to test trigger tuning: it runs the full analysis
including the Claude calls, prints each alert, and posts nothing.

## Cost

Each cycle makes up to four Claude calls (lineup, waiver, trade, plus retries).
At the 3-hour default inside a 15-hour window that's ~5 cycles/day. Turning off
`trade` and `waiver` — the two that send whole-league rosters in the prompt —
cuts most of the token spend if you only care about lineup and injury alerts.
