#!/usr/bin/env bash
# Build script for the Rustyboi iOS app.
#
# Two backends, picked automatically from the host OS:
#
#   macOS  -> XcodeGen + xcodebuild (the native path).
#   Linux  -> the USA-RedDragon/rust-cross container, which ships the iOS SDK
#             sysroots ($IOS_SDKROOT / $IOS_SIMULATOR_SDKROOT) + an osxcross
#             cctools ld64, so the whole app links with no macOS and no Xcode.
#             Builds device (arm64) and simulator (arm64); output is UNSIGNED
#             (no signer in the image yet — add rcodesign for device installs).
#
# In both cases the app is a thin UIKit shell (ios/Sources/main.m) that links
# the Rust staticlib (librustyboi_platform_lib.a) and calls `rustyboi_ios_main`;
# winit's UIKit backend drives everything from there.
#
# Prerequisites:
#   macOS: Xcode (xcodebuild + iOS SDK), xcodegen (brew install xcodegen),
#          rustup target add aarch64-apple-ios[-sim].
#   Linux: Docker or Podman (the image supplies clang/ld/SDK + the rust target).
#
# Usage:
#   ./build-ios.sh                 # device (arm64), Release
#   ./build-ios.sh --simulator     # arm64 iOS Simulator build
#   ./build-ios.sh --debug         # Debug configuration
#   DEVELOPMENT_TEAM=ABCDE12345 ./build-ios.sh   # signed device build (macOS)
#
# Output:
#   macOS: ios/build/Build/Products/<config>-<sdk>/rustyboi.app
#   Linux: ios/build/linux/<sdk>/rustyboi.app  (+ rustyboi.ipa for device)

set -euo pipefail

SDK="iphoneos"
RUST_TARGET="aarch64-apple-ios"
CONFIG="Release"
for arg in "$@"; do
    case "$arg" in
        --simulator) SDK="iphonesimulator"; RUST_TARGET="aarch64-apple-ios-sim" ;;
        --debug)     CONFIG="Debug" ;;
        -h|--help)   sed -n '2,35p' "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg"; echo "Usage: $0 [--simulator] [--debug]"; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."
cd "$PROJECT_ROOT"

# ============================ Linux / container path ============================
# The Rust lib already cross-compiles for aarch64-apple-ios[-sim] in the image;
# the only extra steps are the final Mach-O link against the iOS SDK frameworks
# and hand-assembling the .app bundle (what xcodebuild does on macOS). The image
# supplies the SDK sysroot(s), an osxcross cctools ld64, a deployment-target-aware
# clang wrapper per SDK, and an xcrun answering for iphoneos/iphonesimulator (so
# cc-rs deps like `ring` find their sysroot with no extra plumbing).
if [ "$(uname -s)" != Darwin ]; then
    # Reuse the shared cross machinery (IMAGE, cargo cache volume, engine pick).
    # shellcheck source=tools/rust-cross.sh
    source "$SCRIPT_DIR/rust-cross.sh"
    rc_engine

    # SDK-specific clang wrapper: each targets its own sysroot (and, for the
    # simulator, the -simulator triple); both honor IPHONEOS_DEPLOYMENT_TARGET.
    if [ "$SDK" = iphonesimulator ]; then IOS_CC=arm64-apple-ios-sim-clang; else IOS_CC=arm64-apple-ios-clang; fi

    # Bundle metadata — single-sourced from ios/project.yml so it can't drift.
    yaml_val() { sed -n "s/.*$1: *\"\{0,1\}\([^\"]*\)\"\{0,1\}.*/\1/p" ios/project.yml | head -1; }
    BUNDLE_ID="$(yaml_val PRODUCT_BUNDLE_IDENTIFIER)"
    MARKETING_VERSION="$(yaml_val MARKETING_VERSION)"
    CURRENT_PROJECT_VERSION="$(yaml_val CURRENT_PROJECT_VERSION)"
    : "${BUNDLE_ID:?could not read PRODUCT_BUNDLE_IDENTIFIER from ios/project.yml}"

    if [ "$CONFIG" = Debug ]; then PROFILE_DIR=debug; CARGO_PROFILE=""; else PROFILE_DIR=release; CARGO_PROFILE="--release"; fi
    LIBDIR="target/$RUST_TARGET/$PROFILE_DIR"

    # Frameworks mirror ios/project.yml's dependency list. objc2's own #[link]
    # directives are also embedded as LC_LINKER_OPTIONs in the .a, so ld64
    # auto-links anything not named here; the explicit list matches Xcode.
    # IPHONEOS_DEPLOYMENT_TARGET drives the wrapper's -target/min-version (14.0:
    # Info.plist declares it and the code uses the iOS-14 UTTypeData API); cc-rs
    # deps (ring) resolve their sysroot through the image's iphoneos-aware xcrun.
    echo "==> [container] cargo build + link app ($RUST_TARGET, $CONFIG)"
    INCMD="set -e
        export IPHONEOS_DEPLOYMENT_TARGET=14.0
        cargo build $CARGO_PROFILE --target $RUST_TARGET -p rustyboi-platform --lib
        test -f $LIBDIR/librustyboi_platform_lib.a || { echo 'staticlib missing'; exit 1; }
        $IOS_CC -c ios/Sources/main.m -o $LIBDIR/rustyboi_main.o
        $IOS_CC $LIBDIR/rustyboi_main.o $LIBDIR/librustyboi_platform_lib.a \
            -liconv -lobjc \
            -framework UIKit -framework QuartzCore -framework Metal \
            -framework Foundation -framework CoreFoundation \
            -framework AVFAudio -framework AudioToolbox -framework CoreAudio \
            -framework UniformTypeIdentifiers \
            -o $LIBDIR/rustyboi
        chown -R $HOST_UIDGID target"
    $ENGINE run --rm \
        -v "$PROJECT_ROOT":/project -w /project \
        -v "$CARGO_VOL":/usr/local/cargo/registry \
        "$IMAGE" sh -c "$INCMD"

    [ -f "$LIBDIR/rustyboi" ] || { echo "ERROR: link produced no executable at $LIBDIR/rustyboi"; exit 1; }

    # ---- assemble the .app on the host (plain file ops, no Apple tools) ----
    echo "==> assemble rustyboi.app"
    OUT="ios/build/linux/$SDK"
    APP="$OUT/rustyboi.app"
    rm -rf "$OUT"; mkdir -p "$APP"
    cp "$LIBDIR/rustyboi" "$APP/rustyboi"
    chmod +x "$APP/rustyboi"
    # Resolve the Xcode build-setting variables xcodebuild would expand.
    sed -e "s|\$(EXECUTABLE_NAME)|rustyboi|g" \
        -e "s|\$(PRODUCT_BUNDLE_IDENTIFIER)|$BUNDLE_ID|g" \
        -e "s|\$(MARKETING_VERSION)|$MARKETING_VERSION|g" \
        -e "s|\$(CURRENT_PROJECT_VERSION)|$CURRENT_PROJECT_VERSION|g" \
        ios/Info.plist > "$APP/Info.plist"

    echo ""
    if [ "$SDK" = iphonesimulator ]; then
        # No .ipa for the simulator — it consumes the .app directly (and can only
        # run on macOS; a Linux sim build is for CI compile-coverage).
        echo "App: $PROJECT_ROOT/$APP  (simulator, arm64)"
        echo "Run on macOS:  xcrun simctl install booted '$APP' && xcrun simctl launch booted $BUNDLE_ID"
    else
        # Package an (unsigned) .ipa: Payload/rustyboi.app zipped at the root.
        rm -rf "$OUT/Payload"; mkdir -p "$OUT/Payload"
        cp -R "$APP" "$OUT/Payload/"
        ( cd "$OUT" && zip -qr rustyboi.ipa Payload && rm -rf Payload )
        echo "App: $PROJECT_ROOT/$APP  (UNSIGNED)"
        echo "IPA: $PROJECT_ROOT/$OUT/rustyboi.ipa  (UNSIGNED)"
        echo "Sign for a device with rcodesign, e.g.:"
        echo "  rcodesign sign --p12-file dev.p12 --code-signature-flags runtime \\"
        echo "    --entitlements-xml-path ent.plist $APP"
        echo "  # then re-zip Payload/ into an .ipa, or 'rcodesign' the .ipa directly."
    fi
    exit 0
fi

# ================================ macOS path ================================
command -v xcodebuild >/dev/null 2>&1 || { echo "ERROR: xcodebuild not found. Install Xcode."; exit 1; }
command -v xcodegen  >/dev/null 2>&1 || { echo "ERROR: xcodegen not found. Install with: brew install xcodegen"; exit 1; }
if ! rustup target list --installed 2>/dev/null | grep -qx "$RUST_TARGET"; then
    echo "ERROR: rust target $RUST_TARGET not installed. Run: rustup target add $RUST_TARGET"; exit 1
fi

# ---------- Build the Rust staticlib ----------
# On macOS the Apple toolchain is native, so no CC/AR/SDKROOT plumbing is needed
# (that's only for cross-compiling from Linux via the rust-cross image).
echo "==> cargo build ($RUST_TARGET, release)"
export IPHONEOS_DEPLOYMENT_TARGET=14.0
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
