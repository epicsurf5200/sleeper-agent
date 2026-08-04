#!/bin/bash
# Build a double-clickable macOS app bundle for the sleeper-agent GUI.
#
#   ./scripts/make-app.sh              → installs "Sleeper Agent.app" in ~/Applications
#   ./scripts/make-app.sh /Applications → install elsewhere
#
# The launcher extends PATH so the `claude` CLI (subscription auth) is found
# when launched from Finder/Dock, and logs to ~/Library/Logs/sleeper-agent.log.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${1:-$HOME/Applications}"
APP="$DEST/Sleeper Agent.app"

echo "==> Building release GUI binary"
cargo build --release --features gui --manifest-path "$REPO/Cargo.toml"

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$REPO/target/release/sa-gui" "$APP/Contents/MacOS/sa-gui"
cp "$REPO/assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/MacOS/launcher" <<'EOF'
#!/bin/bash
# Finder launches apps with a minimal PATH — add the usual suspects so the
# `claude` CLI (subscription auth) and homebrew tools are reachable.
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
LOG="$HOME/Library/Logs/sleeper-agent.log"
mkdir -p "$(dirname "$LOG")"
echo "--- launch $(date) ---" >> "$LOG"
exec "$(dirname "$0")/sa-gui" >> "$LOG" 2>&1
EOF
chmod +x "$APP/Contents/MacOS/launcher"

cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Sleeper Agent</string>
    <key>CFBundleDisplayName</key>     <string>Sleeper Agent</string>
    <key>CFBundleIdentifier</key>      <string>dev.sleeper-agent.gui</string>
    <key>CFBundleVersion</key>         <string>0.1.0</string>
    <key>CFBundleShortVersionString</key> <string>0.1.0</string>
    <key>CFBundleExecutable</key>      <string>launcher</string>
    <key>CFBundleIconFile</key>        <string>AppIcon</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
</dict>
</plist>
EOF

# Seed the user-level config from the repo copy so the app works when
# launched from Finder (cwd is / there, so ./config.yaml won't be found).
CFG_DIR="$HOME/Library/Application Support/sleeper-agent"
if [ ! -f "$CFG_DIR/config.yaml" ] && [ -f "$REPO/config.yaml" ]; then
    echo "==> Seeding $CFG_DIR/config.yaml from repo config"
    mkdir -p "$CFG_DIR"
    cp "$REPO/config.yaml" "$CFG_DIR/config.yaml"
fi

echo "==> Done: $APP"
echo "    Launch from Finder/Spotlight, or: open \"$APP\""
echo "    Logs: ~/Library/Logs/sleeper-agent.log"
