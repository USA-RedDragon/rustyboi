#!/usr/bin/env bash
# Build script for the Rustyboi Android APK (Gradle + cargo-ndk).
#
# Prerequisites:
#   - rustup target add aarch64-linux-android
#   - cargo install cargo-ndk
#   - Android SDK with build-tools and platform 34
#   - Android NDK 26.1.10909125 (matches ndkVersion in android/app/build.gradle.kts)
#   - ANDROID_HOME (or ANDROID_SDK_ROOT)
#
# Usage:
#   ./build-android.sh           # debug build
#   ./build-android.sh --release # release build
#                                # Signed if android/keystore.properties exists;
#                                # otherwise produces app-release-unsigned.apk.
#                                # See android/app/build.gradle.kts for format.

set -euo pipefail

BUILD_PROFILE="debug"
GRADLE_TASK="assembleDebug"
for arg in "$@"; do
    case "$arg" in
        --release) BUILD_PROFILE="release"; GRADLE_TASK="assembleRelease" ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        *) echo "Unknown argument: $arg"; echo "Usage: $0 [--release]"; exit 1 ;;
    esac
done

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_ROOT"

# ---------- Preflight ----------

if ! command -v cargo-ndk &> /dev/null; then
    echo "ERROR: cargo-ndk is not installed. Install it with: cargo install cargo-ndk"
    exit 1
fi

if ! rustup target list --installed 2>/dev/null | grep -q '^aarch64-linux-android$'; then
    echo "ERROR: aarch64-linux-android Rust target not installed."
    echo "  Install with: rustup target add aarch64-linux-android"
    exit 1
fi

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

echo "Building rustyboi for Android ($BUILD_PROFILE)..."
( cd android && ./gradlew --warning-mode all ":app:$GRADLE_TASK" )

APK="$PROJECT_ROOT/android/app/build/outputs/apk/$BUILD_PROFILE/app-$BUILD_PROFILE.apk"
if [ ! -f "$APK" ]; then
    # Release variant produces app-release-unsigned.apk if no signingConfig is wired.
    UNSIGNED="$PROJECT_ROOT/android/app/build/outputs/apk/$BUILD_PROFILE/app-$BUILD_PROFILE-unsigned.apk"
    if [ -f "$UNSIGNED" ]; then
        APK="$UNSIGNED"
    fi
fi

echo ""
if [ -f "$APK" ]; then
    echo "APK: $APK"
    echo ""
    echo "Install:  adb install -r $APK"
    echo "Logs:     adb logcat -s rustyboi"
else
    echo "WARNING: Gradle reported success but no APK was found under android/app/build/outputs/apk/$BUILD_PROFILE/"
    exit 1
fi
