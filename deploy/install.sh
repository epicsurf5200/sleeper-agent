#!/usr/bin/env bash
# Install sleeper-agent as a systemd service on a Debian/Ubuntu host or
# Proxmox LXC container. Run as root, from a checkout of this repo:
#
#   sudo ./deploy/install.sh
#
# Idempotent: re-run it after `git pull` to redeploy the binary.
set -euo pipefail

PREFIX=/opt/sleeper-agent
CONF_DIR=/etc/sleeper-agent
CACHE_DIR=/var/cache/sleeper-agent
SERVICE_USER=sleeper
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[[ $EUID -eq 0 ]] || { echo "run as root (sudo $0)" >&2; exit 1; }

# --- build ------------------------------------------------------------------
# Headless: no GUI, so skip the egui/winit dependency tree entirely. That also
# means the container needs no X11/Wayland packages.
if [[ ! -x "$REPO_ROOT/target/release/sa" ]]; then
  command -v cargo >/dev/null || {
    echo "cargo not found. Install Rust 1.78+ (https://rustup.rs) or copy a" >&2
    echo "prebuilt ./target/release/sa into the checkout before running." >&2
    exit 1
  }
  echo "==> building sa (headless)"
  ( cd "$REPO_ROOT" && cargo build --release --bin sa --no-default-features )
fi

# --- user and directories ---------------------------------------------------
if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
  echo "==> creating service user $SERVICE_USER"
  useradd --system --home-dir "$PREFIX" --shell /usr/sbin/nologin "$SERVICE_USER"
fi

install -d -o root -g root -m 0755 "$PREFIX"
install -d -o root -g "$SERVICE_USER" -m 0750 "$CONF_DIR"
install -d -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0750 "$CACHE_DIR"

echo "==> installing binary to $PREFIX/sa"
install -o root -g root -m 0755 "$REPO_ROOT/target/release/sa" "$PREFIX/sa"

# --- config -----------------------------------------------------------------
if [[ ! -f "$CONF_DIR/config.yaml" ]]; then
  echo "==> seeding $CONF_DIR/config.yaml (edit sleeper.username before starting)"
  install -o root -g "$SERVICE_USER" -m 0640 \
    "$REPO_ROOT/examples/config.yaml" "$CONF_DIR/config.yaml"
fi
if [[ ! -f "$CONF_DIR/sleeper-agent.env" ]]; then
  echo "==> seeding $CONF_DIR/sleeper-agent.env (add your secrets)"
  install -o root -g "$SERVICE_USER" -m 0640 \
    "$REPO_ROOT/deploy/sleeper-agent.env.example" "$CONF_DIR/sleeper-agent.env"
fi

# Any extra context files (league rules, keeper notes) referenced by
# settings.context_files resolve against the config's directory.
for f in "$REPO_ROOT"/league-rules.md; do
  [[ -f "$f" && ! -f "$CONF_DIR/$(basename "$f")" ]] || continue
  echo "==> copying $(basename "$f") to $CONF_DIR"
  install -o root -g "$SERVICE_USER" -m 0640 "$f" "$CONF_DIR/"
done

# --- systemd ----------------------------------------------------------------
echo "==> installing systemd units"
install -m 0644 "$REPO_ROOT/deploy/sleeper-agent.service"      /etc/systemd/system/
install -m 0644 "$REPO_ROOT/deploy/sleeper-agent-once.service" /etc/systemd/system/
install -m 0644 "$REPO_ROOT/deploy/sleeper-agent.timer"        /etc/systemd/system/
systemctl daemon-reload

cat <<EOF

Installed. Next:

  1. \$EDITOR $CONF_DIR/config.yaml          # set sleeper.username
  2. \$EDITOR $CONF_DIR/sleeper-agent.env    # ANTHROPIC_API_KEY, SA_WEBHOOK_URL, TZ
  3. sudo -u $SERVICE_USER env \$(grep -v '^#' $CONF_DIR/sleeper-agent.env | xargs) \\
       $PREFIX/sa daemon --once --dry-run    # smoke test: prints alerts, sends none
  4. systemctl enable --now sleeper-agent    # long-running loop
     # or, to let systemd schedule instead:
     # systemctl enable --now sleeper-agent.timer

  journalctl -u sleeper-agent -f             # watch it work
EOF
