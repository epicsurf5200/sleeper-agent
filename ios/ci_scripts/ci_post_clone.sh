#!/bin/sh
# Xcode Cloud runs this after cloning, before xcodebuild. The Rust core has
# to exist as a static library by then, and the project has to be regenerated
# from project.yml, so this does what ./ios/build.sh does locally — minus the
# upload, which Xcode Cloud's TestFlight post-action handles.
set -eu

echo "[ci_post_clone] repo: ${CI_PRIMARY_REPOSITORY_PATH:?}"

if [ -x /opt/homebrew/bin/brew ]; then
    eval "$(/opt/homebrew/bin/brew shellenv)"
elif [ -x /usr/local/bin/brew ]; then
    eval "$(/usr/local/bin/brew shellenv)"
fi

# Rust is not on the Xcode Cloud image. A minimal stable toolchain is enough;
# build.sh adds the aarch64-apple-ios target itself.
if ! command -v cargo >/dev/null 2>&1; then
    echo "[ci_post_clone] installing rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
echo "[ci_post_clone] rust: $(rustc --version)"

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "[ci_post_clone] installing xcodegen"
    brew install xcodegen
fi

# Xcode Cloud manages its own build number, which is also what it stamps
# on the archive; passing it here keeps Local.xcconfig consistent with that.
cd "$CI_PRIMARY_REPOSITORY_PATH"
BUILD_NUMBER="${CI_BUILD_NUMBER:-}" ./ios/build.sh

echo "[ci_post_clone] done"
