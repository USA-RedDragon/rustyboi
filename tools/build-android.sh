#!/usr/bin/env bash
# Build script for the Rustyboi Android app (Gradle + cargo-ndk).
#
# Release builds emit one standalone APK per ABI
# (ABI splits) for sideload / Obtainium / F-Droid; --bundle emits an .aab for
# Google Play.
#
# Prerequisites:
#   - rustup target add aarch64-linux-android x86_64-linux-android armv7-linux-androideabi i686-linux-android
#   - cargo install cargo-ndk
#   - Android SDK with build-tools and platform 37
#   - Android NDK 27.3.13750724 (matches ndkVersion in android/app/build.gradle.kts)
#   - ANDROID_HOME (or ANDROID_SDK_ROOT)
#
# Usage:
#   ./build-android.sh             # debug build (per-ABI APKs)
#   ./build-android.sh --release   # release per-ABI APKs (sideload/Obtainium/F-Droid)
#   ./build-android.sh --bundle    # release .aab (Google Play)
#   ./build-android.sh --release --bundle  # both artifacts
#                                # Release artifacts are signed if
#                                # android/keystore.properties exists; otherwise
#                                # unsigned. See android/app/build.gradle.kts.

set -euo pipefail

BUILD_PROFILE="debug"
GRADLE_TASKS=()
for arg in "$@"; do
    case "$arg" in
        --release) BUILD_PROFILE="release"; GRADLE_TASKS+=("assembleRelease") ;;
        --bundle)  BUILD_PROFILE="release"; GRADLE_TASKS+=("bundleRelease") ;;
        -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg"; echo "Usage: $0 [--release] [--bundle]"; exit 1 ;;
    esac
done
# Default to a debug APK build when no artifact flag is given.
if [ "${#GRADLE_TASKS[@]}" -eq 0 ]; then GRADLE_TASKS=("assembleDebug"); fi

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)/.."
cd "$PROJECT_ROOT"

# ---------- Preflight ----------

if ! command -v cargo-ndk &> /dev/null; then
    echo "ERROR: cargo-ndk is not installed. Install it with: cargo install cargo-ndk"
    exit 1
fi

for tgt in aarch64-linux-android x86_64-linux-android armv7-linux-androideabi i686-linux-android; do
    if ! rustup target list --installed 2>/dev/null | grep -q "^$tgt$"; then
        echo "ERROR: $tgt Rust target not installed."
        echo "  Install with: rustup target add $tgt"
        exit 1
    fi
done

ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [ -z "$ANDROID_HOME" ] || [ ! -d "$ANDROID_HOME" ]; then
    echo "ERROR: ANDROID_HOME (or ANDROID_SDK_ROOT) is not set or invalid."
    exit 1
fi
export ANDROID_HOME

# Locate an NDK. AGP also resolves this via ndkVersion + sdk.dir, but
# cargo-ndk needs ANDROID_NDK_ROOT/ANDROID_NDK_HOME explicitly.
NDK_ROOT="${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-${NDK_HOME:-}}}"
if [ -z "$NDK_ROOT" ] && [ -d "$ANDROID_HOME/ndk" ]; then
    NDK_ROOT=$(ls -d "$ANDROID_HOME/ndk/"*/ 2>/dev/null | sort -V | tail -1 || true)
fi
if [ -z "${NDK_ROOT:-}" ] && [ -d "$ANDROID_HOME/ndk-bundle" ]; then
    NDK_ROOT="$ANDROID_HOME/ndk-bundle"
fi
NDK_ROOT="${NDK_ROOT%/}"
if [ -z "$NDK_ROOT" ] || [ ! -d "$NDK_ROOT" ]; then
    echo "ERROR: Could not locate Android NDK."
    echo "Set ANDROID_NDK_ROOT/ANDROID_NDK_HOME or install via sdkmanager:"
    echo "  sdkmanager 'ndk;26.1.10909125'"
    exit 1
fi
export ANDROID_NDK_ROOT="$NDK_ROOT"
export ANDROID_NDK_HOME="$NDK_ROOT"
echo "Using NDK: $NDK_ROOT"

if [ ! -x "$PROJECT_ROOT/android/gradlew" ]; then
    echo "ERROR: $PROJECT_ROOT/android/gradlew is missing or not executable."
    echo "  Bootstrap once with:"
    echo "    (cd android && gradle wrapper --gradle-version 8.10)"
    echo "    chmod +x android/gradlew"
    exit 1
fi

# ---------- Build ----------

echo "Building rustyboi for Android ($BUILD_PROFILE): ${GRADLE_TASKS[*]}..."
GRADLE_ARGS=()
for t in "${GRADLE_TASKS[@]}"; do GRADLE_ARGS+=(":app:$t"); done
( cd android && ./gradlew --warning-mode all "${GRADLE_ARGS[@]}" )

echo ""
FOUND=0

# APK artifacts (from assemble*). ABI splits produce one standalone APK per ABI
# (app-<abi>-<profile>[-unsigned].apk); there is no combined app-<profile>.apk.
APK_DIR="$PROJECT_ROOT/android/app/build/outputs/apk/$BUILD_PROFILE"
mapfile -t APKS < <(ls "$APK_DIR"/app-*-"$BUILD_PROFILE".apk "$APK_DIR"/app-*-"$BUILD_PROFILE"-unsigned.apk 2>/dev/null)
if [ "${#APKS[@]}" -gt 0 ]; then
    FOUND=1
    echo "APKs (one standalone APK per ABI — install the one matching the device):"
    for a in "${APKS[@]}"; do echo "  $a"; done
    echo ""
    # If a device is attached, resolve its primary ABI and point at that APK.
    DEV_ABI=$(adb shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r')
    MATCH=""
    if [ -n "$DEV_ABI" ]; then
        for a in "${APKS[@]}"; do case "$a" in *"$DEV_ABI"*) MATCH="$a";; esac; done
    fi
    if [ -n "$MATCH" ]; then
        echo "Install (device is $DEV_ABI):  adb install -r $MATCH"
    else
        echo "Install:  adb install -r <apk-for-your-device-abi>"
    fi
    echo "Logs:     adb logcat -s rustyboi"
    echo ""
fi

# App Bundle artifact (from bundle*): a single .aab for Google Play. It carries
# every ABI; Play generates the per-device base + config splits on download.
AAB="$PROJECT_ROOT/android/app/build/outputs/bundle/$BUILD_PROFILE/app-$BUILD_PROFILE.aab"
if [ -f "$AAB" ]; then
    FOUND=1
    echo "AAB (upload to Google Play): $AAB"
fi

if [ "$FOUND" -eq 0 ]; then
    echo "WARNING: Gradle reported success but no APK/AAB was found for $BUILD_PROFILE."
    exit 1
fi
