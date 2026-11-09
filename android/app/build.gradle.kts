plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// The UI is the same HTML/JS as the desktop app (app/ui), plus android.js (the bridge).
val copyUi by tasks.registering(Copy::class) {
    from("../../app/ui") {
        filesMatching("index.html") {
            filter { line ->
                line.replace("<script src=\"i18n.js\"></script>", "<script src=\"android.js\"></script><script src=\"i18n.js\"></script>")
            }
        }
    }
    from("src/main/web")
    into(layout.buildDirectory.dir("generated/ui/ui"))
}

android {
    namespace = "io.github.loopch.mosaic"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.loopch.mosaic"
        minSdk = 26
        targetSdk = 35
        versionCode = 800
        versionName = "0.8.0"
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    signingConfigs {
        // Release signing from CI secrets when present; otherwise the debug key.
        create("release") {
            val ks = System.getenv("MOSAIC_KEYSTORE")
            if (ks != null && file(ks).exists()) {
                storeFile = file(ks)
                storePassword = System.getenv("MOSAIC_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("MOSAIC_KEY_ALIAS")
                keyPassword = System.getenv("MOSAIC_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            val release = signingConfigs.getByName("release")
            signingConfig = if (release.storeFile != null) release else signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    packaging {
        // Engines are executables shipped as lib*.so: they must be extracted to
        // nativeLibraryDir to be runnable.
        jniLibs { useLegacyPackaging = true }
    }

    sourceSets["main"].assets.srcDir(layout.buildDirectory.dir("generated/ui"))
}

tasks.named("preBuild") { dependsOn(copyUi) }

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("androidx.webkit:webkit:1.12.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
}
