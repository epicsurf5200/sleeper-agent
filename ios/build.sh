#!/usr/bin/env bash
# Build the iPhone app: Rust core -> static libs -> Xcode project.
#
#   ./ios/build.sh              # libs + generate SleeperAgent.xcodeproj
#   ./ios/build.sh --open       # ...and open it in Xcode
#   ./ios/build.sh --sim        # also build for the simulator
#   ./ios/build.sh --debug      # debug profile (much faster to compile)
#
# Then in Xcode: pick your device, set a signing team, and hit Run.
set -euo pipefail

IOS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$IOS_DIR/.." && pwd)"
PROFILE="release"
BUILD_SIM=0
OPEN=0

for arg in "$@"; do
    case "$arg" in
        --debug) PROFILE="debug" ;;
        --sim)   BUILD_SIM=1 ;;
        --open)  OPEN=1 ;;
        -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

# rustup is a multiplexer that dispatches on argv[0]; on some installs the
# `rustup` link is missing even though the binary is there. Recreate it in a
# temp dir rather than touching the user's toolchain.
rustup_bin() {
    if command -v rustup >/dev/null 2>&1; then
        command -v rustup
        return
    fi
    local shim="${TMPDIR:-/tmp}/sa-rustup/rustup"
    mkdir -p "$(dirname "$shim")"
    ln -sf "$(command -v cargo)" "$shim"
    echo "$shim"
}

RUSTUP="$(rustup_bin)"

echo "==> Ensuring iOS targets"
TARGETS=(aarch64-apple-ios)
[ "$BUILD_SIM" -eq 1 ] && TARGETS+=("$([ "$(uname -m)" = "arm64" ] && echo aarch64-apple-ios-sim || echo x86_64-apple-ios)")
for t in "${TARGETS[@]}"; do
    "$RUSTUP" target add "$t" >/dev/null 2>&1 || true
done

# Match the app's deployment target, or the linker warns on every object file
# that the library was built for a newer iOS than it is being linked into.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"

FLAGS=(-p sa_ffi)
[ "$PROFILE" = "release" ] && FLAGS+=(--release)

for t in "${TARGETS[@]}"; do
    echo "==> Building sa_ffi for $t ($PROFILE)"
    ( cd "$REPO" && cargo build "${FLAGS[@]}" --target "$t" )
done

# Xcode looks these up by $(PLATFORM_NAME), so lay them out to match.
echo "==> Staging static libraries"
rm -rf "$IOS_DIR/build/lib"
mkdir -p "$IOS_DIR/build/lib/iphoneos"
cp "$REPO/target/aarch64-apple-ios/$PROFILE/libsa_ffi.a" "$IOS_DIR/build/lib/iphoneos/"
if [ "$BUILD_SIM" -eq 1 ]; then
    mkdir -p "$IOS_DIR/build/lib/iphonesimulator"
    simtarget="${TARGETS[1]}"
    cp "$REPO/target/$simtarget/$PROFILE/libsa_ffi.a" "$IOS_DIR/build/lib/iphonesimulator/"
fi

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "==> Installing XcodeGen (generates the .pbxproj from project.yml)"
    if command -v brew >/dev/null 2>&1; then
        brew install xcodegen
    else
        echo "!! XcodeGen not found and Homebrew is unavailable." >&2
        echo "   Install it from https://github.com/yonaskolb/XcodeGen and re-run." >&2
        exit 1
    fi
fi

echo "==> Generating Xcode project"
( cd "$IOS_DIR" && xcodegen generate --quiet )

echo
echo "==> Done: $IOS_DIR/SleeperAgent.xcodeproj"
echo "    Open it, select your iPhone, set Signing & Capabilities -> Team,"
echo "    then Run. A free Apple ID works but the build expires after 7 days."
[ "$OPEN" -eq 1 ] && open "$IOS_DIR/SleeperAgent.xcodeproj"
exit 0
