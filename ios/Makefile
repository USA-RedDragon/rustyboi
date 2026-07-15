# iOS app build. Invoked from the root Makefile as `make ios` (SIMULATOR=1 /
# DEBUG=1); not meant to be run standalone.
#
# Builds device (arm64) or simulator (arm64);
# output is UNSIGNED (add rcodesign for device installs).
# The app is a thin UIKit shell (ios/Sources/main.m) linking the Rust staticlib
# (librustyboi_platform_lib.a) and calling rustyboi_ios_main.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
ROOT := $(realpath $(CURDIR)/..)

.PHONY: build
build:
	@cd "$(ROOT)"
	PROJECT_ROOT="$$(pwd)"
	SDK="iphoneos"; RUST_TARGET="aarch64-apple-ios"; CONFIG="Release"
	if [ -n "$(SIMULATOR)" ]; then SDK="iphonesimulator"; RUST_TARGET="aarch64-apple-ios-sim"; fi
	if [ -n "$(DEBUG)" ]; then CONFIG="Debug"; fi

	source tools/rust-cross.sh
	rc_engine
	if [ "$$SDK" = iphonesimulator ]; then IOS_CC=arm64-apple-ios-sim-clang; else IOS_CC=arm64-apple-ios-clang; fi
	# Bundle metadata single-sourced from ios/project.yml so it can't drift.
	yaml_val() { sed -n "s/.*$$1: *\"\{0,1\}\([^\"]*\)\"\{0,1\}.*/\1/p" ios/project.yml | head -1; }
	BUNDLE_ID="$$(yaml_val PRODUCT_BUNDLE_IDENTIFIER)"
	MARKETING_VERSION="$$(yaml_val MARKETING_VERSION)"
	CURRENT_PROJECT_VERSION="$$(yaml_val CURRENT_PROJECT_VERSION)"
	: "$${BUNDLE_ID:?could not read PRODUCT_BUNDLE_IDENTIFIER from ios/project.yml}"
	if [ "$$CONFIG" = Debug ]; then PROFILE_DIR=debug; CARGO_PROFILE=""; else PROFILE_DIR=release; CARGO_PROFILE="--release"; fi
	LIBDIR="target/$$RUST_TARGET/$$PROFILE_DIR"

	# ld64 auto-links objc2's LC_LINKER_OPTIONs; the explicit -framework list
	# mirrors ios/project.yml. IPHONEOS_DEPLOYMENT_TARGET=14.0 drives the
	# wrapper's -target/min-version (Info.plist + iOS-14 UTTypeData API).
	echo "==> [container] cargo build + link app ($$RUST_TARGET, $$CONFIG)"
	INCMD="set -e
	    export IPHONEOS_DEPLOYMENT_TARGET=14.0
	    export RUSTFLAGS=\"\$$(make -s pgo-flags 2>/dev/null || true) \$${RUSTFLAGS:-}\"
	    cargo build $$CARGO_PROFILE --target $$RUST_TARGET -p rustyboi-platform --lib
	    test -f $$LIBDIR/librustyboi_platform_lib.a || { echo 'staticlib missing'; exit 1; }
	    $$IOS_CC -c ios/Sources/main.m -o $$LIBDIR/rustyboi_main.o
	    $$IOS_CC $$LIBDIR/rustyboi_main.o $$LIBDIR/librustyboi_platform_lib.a \
	        -liconv -lobjc \
	        -framework UIKit -framework QuartzCore -framework Metal \
	        -framework Foundation -framework CoreFoundation \
	        -framework AVFAudio -framework AudioToolbox -framework CoreAudio \
	        -framework UniformTypeIdentifiers \
	        -o $$LIBDIR/rustyboi
	    chown -R $$HOST_UIDGID target"
	"$$ENGINE" run --rm \
	    -v "$$PROJECT_ROOT":/project -w /project \
	    -v "$$CARGO_VOL":/usr/local/cargo/registry \
	    "$$IMAGE" sh -c "$$INCMD"
	[ -f "$$LIBDIR/rustyboi" ] || { echo "ERROR: link produced no executable at $$LIBDIR/rustyboi"; exit 1; }

	# ---- assemble the .app on the host (plain file ops, no Apple tools) ----
	echo "==> assemble rustyboi.app"
	OUT="ios/build/linux/$$SDK"; APP="$$OUT/rustyboi.app"
	rm -rf "$$OUT"; mkdir -p "$$APP"
	cp "$$LIBDIR/rustyboi" "$$APP/rustyboi"; chmod +x "$$APP/rustyboi"
	# Resolve the Xcode build-setting variables xcodebuild would expand.
	sed -e "s|\$$(EXECUTABLE_NAME)|rustyboi|g" \
	    -e "s|\$$(PRODUCT_BUNDLE_IDENTIFIER)|$$BUNDLE_ID|g" \
	    -e "s|\$$(MARKETING_VERSION)|$$MARKETING_VERSION|g" \
	    -e "s|\$$(CURRENT_PROJECT_VERSION)|$$CURRENT_PROJECT_VERSION|g" \
	    ios/Info.plist > "$$APP/Info.plist"
	echo ""
	if [ "$$SDK" = iphonesimulator ]; then
	  echo "App: $$PROJECT_ROOT/$$APP  (simulator, arm64)"
	  echo "Run on macOS:  xcrun simctl install booted '$$APP' && xcrun simctl launch booted $$BUNDLE_ID"
	else
	  # Package an (unsigned) .ipa: Payload/rustyboi.app zipped at the root.
	  rm -rf "$$OUT/Payload"; mkdir -p "$$OUT/Payload"
	  cp -R "$$APP" "$$OUT/Payload/"
	  ( cd "$$OUT" && zip -qr rustyboi.ipa Payload && rm -rf Payload )
	  echo "App: $$PROJECT_ROOT/$$APP  (UNSIGNED)"
	  echo "IPA: $$PROJECT_ROOT/$$OUT/rustyboi.ipa  (UNSIGNED)"
	  echo "Sign for a device with rcodesign, e.g.:"
	  echo "  rcodesign sign --p12-file dev.p12 --code-signature-flags runtime \\"
	  echo "    --entitlements-xml-path ent.plist $$APP"
	  echo "  # then re-zip Payload/ into an .ipa, or 'rcodesign' the .ipa directly."
	fi
