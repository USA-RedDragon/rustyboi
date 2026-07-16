// Pure JVM module: the ROM-library scan helpers use only java.util.zip +
// Kotlin stdlib, so they compile and unit-test with a plain JDK — no Android
// SDK/NDK. This is what the `android / coverage` CI job builds, which is why it
// needs no android-37 platform.
plugins {
    id("org.jetbrains.kotlin.jvm")
    jacoco
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}

tasks.named<Test>("test") {
    useJUnit()
}

// Turn the JUnit exec data into the XML report Codecov ingests (flags: android).
tasks.named<JacocoReport>("jacocoTestReport") {
    dependsOn("test")
    reports {
        xml.required.set(true)
        html.required.set(false)
        csv.required.set(false)
    }
}
