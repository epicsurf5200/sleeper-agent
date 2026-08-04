#!/bin/bash
# Launch the sleeper-agent GUI.
#
#   ./scripts/launch.sh          → launch the installed app bundle (builds it if missing)
#   ./scripts/launch.sh --tui    → run the terminal UI instead
#   ./scripts/launch.sh --dev    → run the GUI from the repo (foreground, logs to terminal)
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$HOME/Applications/Sleeper Agent.app"

case "${1:-}" in
    --tui)
        exec cargo run --manifest-path "$REPO/Cargo.toml" --bin sa -- ui
        ;;
    --dev)
        exec cargo run --manifest-path "$REPO/Cargo.toml" --features gui --bin sa-gui
        ;;
    "")
        if [ ! -d "$APP" ]; then
            echo "==> App not installed yet — building it first"
            "$REPO/scripts/make-app.sh"
        fi
        open "$APP"
        echo "==> Launched Sleeper Agent (logs: ~/Library/Logs/sleeper-agent.log)"
        ;;
    *)
        echo "usage: $0 [--tui|--dev]" >&2
        exit 1
        ;;
esac
