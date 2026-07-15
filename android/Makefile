# Android app build (Gradle + cargo-ndk). Invoked from the root Makefile as
# `make android` (RELEASE=1 / BUNDLE=1); not meant to be run standalone.
#   Release builds emit one standalone APK per ABI (sideload/Obtainium/F-Droid);
#   BUNDLE=1 emits an .aab for Google Play. Release artifacts are signed if
#   android/keystore.properties exists, else unsigned.
# Prereqs: rustup android targets, cargo-ndk, Android SDK + NDK, ANDROID_HOME.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
ROOT := $(realpath $(CURDIR)/..)

.PHONY: build
build:
	@cd "$(ROOT)"
	PROJECT_ROOT="$$(pwd)"
	BUILD_PROFILE=debug; GRADLE_TASKS=()
	if [ -n "$(RELEASE)" ]; then BUILD_PROFILE=release; GRADLE_TASKS+=("assembleRelease"); fi
	if [ -n "$(BUNDLE)" ]; then BUILD_PROFILE=release; GRADLE_TASKS+=("bundleRelease"); fi
	if [ $${#GRADLE_TASKS[@]} -eq 0 ]; then GRADLE_TASKS=("assembleDebug"); fi

	# ---------- Preflight ----------
	if [ -z "$(SKIP_RUST)" ] && ! command -v cargo-ndk >/dev/null 2>&1; then
	  echo "ERROR: cargo-ndk is not installed. Install it with: cargo install cargo-ndk"; exit 1
	fi
	for tgt in aarch64-linux-android x86_64-linux-android armv7-linux-androideabi i686-linux-android; do
	  if ! rustup target list --installed 2>/dev/null | grep -q "^$$tgt$$"; then
	    echo "ERROR: $$tgt Rust target not installed."; echo "  Install with: rustup target add $$tgt"; exit 1
	  fi
	done
	ANDROID_HOME="$${ANDROID_HOME:-$${ANDROID_SDK_ROOT:-}}"
	if [ -z "$$ANDROID_HOME" ] || [ ! -d "$$ANDROID_HOME" ]; then
	  echo "ERROR: ANDROID_HOME (or ANDROID_SDK_ROOT) is not set or invalid."; exit 1
	fi
	export ANDROID_HOME
	# cargo-ndk needs ANDROID_NDK_ROOT/HOME explicitly (AGP resolves via ndkVersion).
	NDK_ROOT="$${ANDROID_NDK_ROOT:-$${ANDROID_NDK_HOME:-$${NDK_HOME:-}}}"
	if [ -z "$$NDK_ROOT" ] && [ -d "$$ANDROID_HOME/ndk" ]; then
	  NDK_ROOT=$$(ls -d "$$ANDROID_HOME/ndk/"*/ 2>/dev/null | sort -V | tail -1 || true)
	fi
	if [ -z "$${NDK_ROOT:-}" ] && [ -d "$$ANDROID_HOME/ndk-bundle" ]; then NDK_ROOT="$$ANDROID_HOME/ndk-bundle"; fi
	NDK_ROOT="$${NDK_ROOT%/}"
	if [ -z "$$NDK_ROOT" ] || [ ! -d "$$NDK_ROOT" ]; then
	  echo "ERROR: Could not locate Android NDK."
	  echo "Set ANDROID_NDK_ROOT/ANDROID_NDK_HOME or install via sdkmanager:"
	  echo "  sdkmanager 'ndk;26.1.10909125'"; exit 1
	fi
	export ANDROID_NDK_ROOT="$$NDK_ROOT" ANDROID_NDK_HOME="$$NDK_ROOT"
	echo "Using NDK: $$NDK_ROOT"
	if [ ! -x "$$PROJECT_ROOT/android/gradlew" ]; then
	  echo "ERROR: $$PROJECT_ROOT/android/gradlew is missing or not executable."
	  echo "  Bootstrap once with:"
	  echo "    (cd android && gradle wrapper --gradle-version 8.10)"
	  echo "    chmod +x android/gradlew"; exit 1
	fi

	# ---------- Build ----------
	echo "Building rustyboi for Android ($$BUILD_PROFILE): $${GRADLE_TASKS[*]}..."
	GRADLE_ARGS=()
	for t in "$${GRADLE_TASKS[@]}"; do GRADLE_ARGS+=(":app:$$t"); done
	if [ -n "$(ABI)" ]; then GRADLE_ARGS+=("-PabiFilter=$(ABI)"); fi
	if [ -n "$(SKIP_RUST)" ]; then GRADLE_ARGS+=(-x :app:buildRustLibRelease -x :app:buildRustLibDebug); fi
	# PGO best-effort: cargo-ndk (run by Gradle) inherits RUSTFLAGS; IR PGO is
	# target-portable to the Android ABIs. RB_NO_PGO=1 opts out.
	export RUSTFLAGS="$$(make -s pgo-flags 2>/dev/null || true) $${RUSTFLAGS:-}"
	( cd android && ./gradlew --warning-mode all "$${GRADLE_ARGS[@]}" )

	echo ""
	FOUND=0
	# ABI splits: one standalone APK per ABI (app-<abi>-<profile>[-unsigned].apk).
	APK_DIR="$$PROJECT_ROOT/android/app/build/outputs/apk/$$BUILD_PROFILE"
	mapfile -t APKS < <(ls "$$APK_DIR"/app-*-"$$BUILD_PROFILE".apk "$$APK_DIR"/app-*-"$$BUILD_PROFILE"-unsigned.apk 2>/dev/null)
	if [ $${#APKS[@]} -gt 0 ]; then
	  FOUND=1
	  echo "APKs (one standalone APK per ABI — install the one matching the device):"
	  for a in "$${APKS[@]}"; do echo "  $$a"; done
	  echo ""
	fi
	# .aab carries every ABI; Play generates per-device splits on download.
	AAB="$$PROJECT_ROOT/android/app/build/outputs/bundle/$$BUILD_PROFILE/app-$$BUILD_PROFILE.aab"
	if [ -f "$$AAB" ]; then FOUND=1; echo "AAB (upload to Google Play): $$AAB"; fi
	if [ "$$FOUND" -eq 0 ]; then
	  echo "WARNING: Gradle reported success but no APK/AAB was found for $$BUILD_PROFILE."; exit 1
	fi
