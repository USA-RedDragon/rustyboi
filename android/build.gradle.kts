// Top-level build file. Plugin versions live here so all modules share them.
plugins {
    id("com.android.application") version "9.3.0" apply false
    // Matches the Kotlin AGP 9.3.0 bundles for the app's built-in Kotlin, so the
    // pure-JVM :romscan module compiles against the same stdlib.
    id("org.jetbrains.kotlin.jvm") version "2.2.10" apply false
}
