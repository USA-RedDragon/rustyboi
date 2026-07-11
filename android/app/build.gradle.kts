import org.gradle.api.tasks.Exec
import org.gradle.kotlin.dsl.register
import java.util.Properties

plugins {
    id("com.android.application")
}

// ---------------------------------------------------------------------------
// Release signing
// ---------------------------------------------------------------------------
//
// Reads from `android/keystore.properties` (or `android/app/keystore.properties`)
// if present so `./build-android.sh --release` produces a signed APK without
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

android {
    namespace = "dev.mcswain.rustyboi"
    compileSdk = 34
    ndkVersion = "26.1.10909125"

    defaultConfig {
        applicationId = "dev.mcswain.rustyboi"
        minSdk = 28
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += "arm64-v8a"
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
            isMinifyEnabled = false
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
    val args = mutableListOf(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "-P", ndkPlatform,
        "-o", jniLibsDir.asFile.absolutePath,
        "build",
        "-p", "rustyboi-platform",
    )
    if (cargoProfile == "release") args += "--release"
    commandLine = args
    // Tune codegen for modern arm64 phones. minSdk 28 (Android 9, 2018+) on
    // arm64-v8a effectively guarantees armv8.1-A LSE atomics. We schedule for
    // Cortex-A55 (LITTLE core in nearly every modern big.LITTLE SoC) so the
    // little cluster stays responsive; +lse/+rcpc raise the ISA floor for
    // faster atomics, which the hot emulator loop benefits from. Release-only
    // because debug builds prioritize compile time.
    if (cargoProfile == "release") {
        environment(
            "CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS",
            "-C target-cpu=cortex-a55 -C target-feature=+lse,+rcpc",
        )
    }
    inputs.dir(rustWorkspaceRoot.dir("rustyboi-platform/src"))
    inputs.dir(rustWorkspaceRoot.dir("rustyboi-core/src"))
    inputs.dir(rustWorkspaceRoot.dir("rustyboi-egui/src"))
    inputs.dir(rustWorkspaceRoot.dir("rustyboi-debugger/src"))
    inputs.file(rustWorkspaceRoot.file("Cargo.toml"))
    inputs.file(rustWorkspaceRoot.file("Cargo.lock"))
    inputs.file(rustWorkspaceRoot.file("rustyboi-platform/Cargo.toml"))
    outputs.dir(jniLibsDir.dir("arm64-v8a"))
}

val buildRustLibDebug = cargoNdkTask("buildRustLibDebug", "debug")
val buildRustLibRelease = cargoNdkTask("buildRustLibRelease", "release")

// We ship no unit or instrumented tests. Disabling their variant components
// stops AGP from creating test components at all — which both trims
// configuration time and avoids AGP 9.2.1's internal use of the deprecated
// Project-object dependency notation when wiring test → main dependencies
// (deprecated in Gradle 9, an error in Gradle 10).
androidComponents.beforeVariants { variantBuilder ->
    (variantBuilder as com.android.build.api.variant.HasUnitTestBuilder).enableUnitTest = false
    (variantBuilder as com.android.build.api.variant.HasAndroidTestBuilder).enableAndroidTest = false
}

androidComponents.onVariants { variant ->
    val taskName = "buildRustLib${variant.name.replaceFirstChar { it.uppercase() }}"
    val provider = if (variant.buildType == "release") buildRustLibRelease else buildRustLibDebug
    tasks.matching { it.name == "merge${variant.name.replaceFirstChar { it.uppercase() }}JniLibFolders" }
        .configureEach { dependsOn(provider) }
    tasks.matching { it.name == "merge${variant.name.replaceFirstChar { it.uppercase() }}NativeLibs" }
        .configureEach { dependsOn(provider) }
    tasks.matching { it.name == "preBuild" }.configureEach { dependsOn(provider) }
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
    implementation("androidx.core:core-ktx:1.13.1")
    // DocumentFile gives us a recursive listFiles() API over SAF tree URIs,
    // used by the ROM library scanner in RustyboiActivity.
    implementation("androidx.documentfile:documentfile:1.1.0")
}
