import org.gradle.api.tasks.Exec
import org.gradle.kotlin.dsl.register
import java.io.File
import java.util.Properties

plugins {
    id("com.android.application")
}

// ---------------------------------------------------------------------------
// Release signing
// ---------------------------------------------------------------------------
//
// Reads from `android/keystore.properties` (or `android/app/keystore.properties`)
// if present so `make android RELEASE=1` produces a signed APK without
// needing to plumb `-Pandroid.injected.signing.*` flags on the command line.
//
// Expected keys:
//   storeFile=/absolute/or/relative/path/to/release.jks
//   storePassword=...
//   keyAlias=...
//   keyPassword=...
//
// File is gitignored. If absent, release builds fall back to unsigned and
// emit a warning at configuration time.
val keystoreProps: Properties? = run {
    val candidates = listOf(
        rootProject.file("keystore.properties"),
        file("keystore.properties"),
    )
    val found = candidates.firstOrNull { it.isFile }
    if (found != null) {
        Properties().apply { found.inputStream().use { load(it) } }
    } else {
        null
    }
}

val allAbis = listOf("arm64-v8a", "x86_64", "armeabi-v7a", "x86")
val selectedAbis: List<String> = (project.findProperty("abiFilter") as String?)
    ?.split(",")?.map(String::trim)?.filter(String::isNotEmpty)
    ?.also { sel -> require(sel.all(allAbis::contains)) { "abiFilter $sel not a subset of $allAbis" } }
    ?: allAbis

android {
    namespace = "dev.mcswain.rustyboi"
    compileSdk = 37
    ndkVersion = "27.3.13750724"

    defaultConfig {
        applicationId = "dev.mcswain.rustyboi"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
    }

    // One APK per ABI instead of a fat APK carrying every native lib. Each
    // device downloads only its own slice; `isUniversalApk = false` means we
    // do NOT also emit a combined APK. The `include` list is the sole ABI
    // scope — AGP forbids also setting `defaultConfig.ndk.abiFilters alongside
    // splits. (For Play multi-APK upload each ABI needs a distinct versionCode
    // — not wired here since we sideload.)
    splits {
        abi {
            isEnable = true
            reset()
            include(*selectedAbis.toTypedArray())
            isUniversalApk = false
        }
    }

    signingConfigs {
        if (keystoreProps != null) {
            create("release") {
                val storePath = keystoreProps.getProperty("storeFile")
                    ?: error("keystore.properties: missing storeFile")
                // Resolve relative paths against the android/ project root.
                storeFile = file(storePath).let {
                    if (it.isAbsolute) it else rootProject.file(storePath)
                }
                storePassword = keystoreProps.getProperty("storePassword")
                    ?: error("keystore.properties: missing storePassword")
                keyAlias = keystoreProps.getProperty("keyAlias")
                    ?: error("keystore.properties: missing keyAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
                    ?: error("keystore.properties: missing keyPassword")
            }
        }
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = true
            if (keystoreProps != null) {
                signingConfig = signingConfigs.getByName("release")
            } else {
                logger.warn(
                    "[rustyboi] No android/keystore.properties found; release APK will be unsigned.\n" +
                    "  Create android/keystore.properties with storeFile/storePassword/keyAlias/keyPassword\n" +
                    "  to produce a signed release APK."
                )
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // The cdylib is staged into `app/src/main/jniLibs/<abi>/librustyboi_platform.so` by the
    // `buildRustLib*` tasks below, then AGP packages it into the APK automatically.
    sourceSets["main"].jniLibs.directories += "src/main/jniLibs"

    // No Java source files yet beyond the activity, which lives at the default location.
    packaging {
        // Native debug symbols are large; strip from release builds.
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

// ---------------------------------------------------------------------------
// cargo-ndk integration
// ---------------------------------------------------------------------------
//
// Each variant gets its own task because cargo profile differs (debug vs release)
// and we want incremental rebuilds when the Rust sources change.

val rustWorkspaceRoot = rootProject.layout.projectDirectory.dir("..")
val jniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")

fun cargoNdkTask(name: String, cargoProfile: String): TaskProvider<Exec> = tasks.register<Exec>(name) {
    workingDir = rustWorkspaceRoot.asFile
    // cpal's Android backend links libaaudio.so directly, which the NDK only
    // ships in its sysroot at API >= 26. cargo-ndk otherwise defaults to API 21
    // (no libaaudio.so → link failure), so pin the link platform to our minSdk.
    val ndkPlatform = (android.defaultConfig.minSdk ?: 26).toString()
    val abis = selectedAbis
    val args = mutableListOf("cargo", "ndk")
    abis.forEach { args += listOf("-t", it) }
    args += listOf(
        "-P", ndkPlatform,
        "-o", jniLibsDir.asFile.absolutePath,
        "build",
        "-p", "rustyboi-platform",
    )
    if (cargoProfile == "release") args += "--release"
    commandLine = args
    // Per-ABI codegen (release only; debug prioritizes compile time). The env
    // vars are target-scoped, so each affects only its own ABI.
    //
    // arm64: schedule for Cortex-A55 (the LITTLE core in nearly every modern
    // big.LITTLE SoC) so the little cluster stays responsive. +outline-atomics
    // uses LSE atomics at runtime when the CPU has them and falls back safely on
    // armv8.0, so the fast path costs no compatibility down to minSdk 26.
    //
    // x86_64: left at baseline (emulators / ChromeOS span a wide CPU range); the
    // arm-only flags above don't apply, so no tuning is forced.
    //
    // 16 KiB .so alignment (required for Play + newer 16 KB-page devices) is not
    // forced here: NDK r27's linker already defaults to max-page-size=16384 for
    // both ABIs (verified: LOAD segments land on 0x4000).
    if (cargoProfile == "release") {
        environment(
            "CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS",
            "-C target-cpu=cortex-a55 -C target-feature=+outline-atomics",
        )
    }
    // Full workspace-crate dependency closure of rustyboi-platform (verify with
    // `cargo metadata`); a crate missing here leaves this task UP-TO-DATE after
    // edits to it, packaging a stale .so. build.rs goes through files() so
    // crates without one are tolerated (and picked up if added later).
    listOf(
        "rustyboi-platform",
        "rustyboi-core",
        "rustyboi-debugger",
        "rustyboi-egui",
        "rustyboi-frontend",
        "rustyboi-session",
    ).forEach { crate ->
        inputs.dir(rustWorkspaceRoot.dir("$crate/src"))
        inputs.file(rustWorkspaceRoot.file("$crate/Cargo.toml"))
        inputs.files(rustWorkspaceRoot.file("$crate/build.rs"))
    }
    // Compiled into rustyboi-frontend via include_wgsl!.
    inputs.dir(rustWorkspaceRoot.dir("rustyboi-frontend/shaders"))
    inputs.file(rustWorkspaceRoot.file("Cargo.toml"))
    inputs.file(rustWorkspaceRoot.file("Cargo.lock"))
    abis.forEach { outputs.dir(jniLibsDir.dir(it)) }
}

val buildRustLibDebug = cargoNdkTask("buildRustLibDebug", "debug")
val buildRustLibRelease = cargoNdkTask("buildRustLibRelease", "release")

// JVM unit tests (JaCoCo coverage) are enabled; instrumented (androidTest)
// tests remain off — we ship none, and disabling their component trims
// configuration time and avoids AGP's internal use of the deprecated
// Project-object dependency notation for the androidTest → main wiring.
androidComponents.beforeVariants { variantBuilder ->
    (variantBuilder as com.android.build.api.variant.HasAndroidTestBuilder).enableAndroidTest = false
}

androidComponents.onVariants { variant ->
    val cap = variant.name.replaceFirstChar { it.uppercase() }
    val provider = if (variant.buildType == "release") buildRustLibRelease else buildRustLibDebug
    tasks.matching { it.name == "merge${cap}JniLibFolders" }.configureEach { dependsOn(provider) }
    tasks.matching { it.name == "merge${cap}NativeLibs" }.configureEach { dependsOn(provider) }
}

tasks.named("clean").configure {
    doLast { jniLibsDir.asFile.deleteRecursively() }
}

dependencies {
    // GameActivity AAR. Version must match the ABI expected by the
    // `android-activity` crate that winit 0.29 pulls in (android-activity 0.5
    // expects games-activity 2.0.x).
    implementation("androidx.games:games-activity:4.4.2")
    // GameActivity transitively extends AppCompatActivity, so its supertypes
    // must be on the compile classpath.
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.core:core-ktx:1.19.0")
    // DocumentFile gives us a recursive listFiles() API over SAF tree URIs,
    // used by the ROM library scanner in RustyboiActivity.
    implementation("androidx.documentfile:documentfile:1.1.0")

    // Pure JVM ROM-scan helpers. Unit tests + JaCoCo coverage for these live in
    // the :romscan module so the coverage CI job needs only a JDK, no Android SDK.
    implementation(project(":romscan"))
}
