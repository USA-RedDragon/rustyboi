#!/usr/bin/env bash
# Build script for the Rustyboi iOS app (XcodeGen + xcodebuild).
#
# Compiles rustyboi-platform to a static library for iOS, generates the Xcode
# project from ios/project.yml, and builds the .app with xcodebuild. macOS-only
# (xcodebuild and the iOS SDK live in Xcode) — the cross-image build path is for
# the *libretro core*; the standalone app needs real Xcode.
#
# Prerequisites:
#   - macOS with Xcode (xcodebuild + the iOS SDK)
#   - rustup target add aarch64-apple-ios          (device)
#   - rustup target add aarch64-apple-ios-sim      (simulator, with --simulator)
#   - xcodegen                                     (brew install xcodegen)
#
# Usage:
#   ./build-ios.sh                 # device (arm64), Release; unsigned unless
#                                  #   DEVELOPMENT_TEAM is set
#   ./build-ios.sh --simulator     # arm64 iOS Simulator build (no signing)
#   ./build-ios.sh --debug         # Debug configuration
#   DEVELOPMENT_TEAM=ABCDE12345 ./build-ios.sh   # signed device build
#
# Output: ios/build/Build/Products/<config>-<sdk>/rustyboi.app

set -euo pipefail

SDK="iphoneos"
RUST_TARGET="aarch64-apple-ios"
CONFIG="Release"
for arg in "$@"; do
    case "$arg" in
        --simulator) SDK="iphonesimulator"; RUST_TARGET="aarch64-apple-ios-sim" ;;
        --debug)     CONFIG="Debug" ;;
        -h|--help)   sed -n '2,22p' "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg"; echo "Usage: $0 [--simulator] [--debug]"; exit 1 ;;
    esac
done

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)/.."
cd "$PROJECT_ROOT"

# ---------- Preflight ----------
[ "$(uname -s)" = Darwin ] || { echo "ERROR: iOS builds require macOS (xcodebuild + the iOS SDK)."; exit 1; }
command -v xcodebuild >/dev/null 2>&1 || { echo "ERROR: xcodebuild not found. Install Xcode."; exit 1; }
command -v xcodegen  >/dev/null 2>&1 || { echo "ERROR: xcodegen not found. Install with: brew install xcodegen"; exit 1; }
if ! rustup target list --installed 2>/dev/null | grep -qx "$RUST_TARGET"; then
    echo "ERROR: rust target $RUST_TARGET not installed. Run: rustup target add $RUST_TARGET"; exit 1
fi

# ---------- Build the Rust staticlib ----------
# On macOS the Apple toolchain is native, so no CC/AR/SDKROOT plumbing is needed
# (that's only for cross-compiling from Linux via the rust-cross image).
echo "==> cargo build ($RUST_TARGET, release)"
cargo build --release -p rustyboi-platform --lib --target "$RUST_TARGET"
RUST_LIB_DIR="$PROJECT_ROOT/target/$RUST_TARGET/release"
[ -f "$RUST_LIB_DIR/librustyboi_platform_lib.a" ] || {
    echo "ERROR: staticlib missing: $RUST_LIB_DIR/librustyboi_platform_lib.a"; exit 1; }

# ---------- Generate the Xcode project ----------
echo "==> xcodegen generate"
( cd ios && xcodegen generate )

# ---------- xcodebuild ----------
DERIVED="$PROJECT_ROOT/ios/build"
SIGN_ARGS=()
if [ "$SDK" = iphonesimulator ]; then
    SIGN_ARGS=(CODE_SIGNING_ALLOWED=NO)          # the simulator never signs
elif [ -n "${DEVELOPMENT_TEAM:-}" ]; then
    SIGN_ARGS=(DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" -allowProvisioningUpdates)
else
    echo "note: DEVELOPMENT_TEAM unset -> building an UNSIGNED device .app (won't install on a device)."
    SIGN_ARGS=(CODE_SIGNING_ALLOWED=NO)
fi

echo "==> xcodebuild ($CONFIG, $SDK)"
xcodebuild \
    -project ios/rustyboi.xcodeproj \
    -scheme rustyboi \
    -configuration "$CONFIG" \
    -sdk "$SDK" \
    -derivedDataPath "$DERIVED" \
    RUST_LIB_DIR="$RUST_LIB_DIR" \
    "${SIGN_ARGS[@]}" \
    build

APP="$DERIVED/Build/Products/$CONFIG-$SDK/rustyboi.app"
echo ""
if [ -d "$APP" ]; then
    echo "App: $APP"
    if [ "$SDK" = iphonesimulator ]; then
        echo "Run in the simulator:"
        echo "  open -a Simulator"
        echo "  xcrun simctl install booted '$APP'"
        echo "  xcrun simctl launch booted dev.mcswain.rustyboi"
    else
        echo "Install on a device (signed build) via Xcode or ios-deploy, or package an .ipa."
    fi
else
    echo "WARNING: xcodebuild reported success but rustyboi.app was not found."
    exit 1
fi
