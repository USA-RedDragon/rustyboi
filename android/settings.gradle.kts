pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // The rustls-platform-verifier Android support library (the JNI
        // CertificateVerifier class its Rust side calls) is shipped as a local
        // .aar in app/libs — without it, HTTPS via the platform verifier crashes.
        flatDir { dirs("app/libs") }
    }
}

rootProject.name = "rustyboi"
include(":app")
