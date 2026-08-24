plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.pydmg.neogeo"
    compileSdk = 34
    // ndkVersion intentionally not pinned here: the .so files are produced
    // out-of-band by `build-android.sh` (cargo ndk) and Gradle just packs
    // whatever is dropped under src/main/jniLibs/<abi>/. Pinning a version
    // would force Gradle to fail when only the SDK (no NDK) is installed,
    // which is the case on CI / minimal dev machines.
    // ndkVersion = "26.1.10909125"

    defaultConfig {
        applicationId = "com.pydmg.neogeo"
        minSdk = 24            // Android 7.0 — covers ~97 % of devices in 2026
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        // Note: abiFilters are NOT pinned. Whichever ABIs the user has
        // populated under src/main/jniLibs/<abi>/ will be packaged. The
        // build-android.sh script populates arm64-v8a + armeabi-v7a + x86_64.
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            // Signing: leave to user. `./gradlew assembleRelease` will
            // produce an UNSIGNED apk; sign with `apksigner` or attach
            // a keystore via `signingConfigs { ... }`.
        }
        debug {
            // Debug builds embed the unstripped .so, faster to iterate.
            isJniDebuggable = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    // The .so files live under app/src/main/jniLibs/<abi>/ — Gradle
    // packs them into the APK automatically. The Rust build script
    // (`build-android.sh` / .bat) writes them there before assemble.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")

    packaging {
        // Keep the .so uncompressed so it can be loaded directly from
        // the APK without extraction (faster cold start + smaller RAM).
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.0")
    implementation("androidx.documentfile:documentfile:1.0.1")
    implementation("androidx.recyclerview:recyclerview:1.3.2")
    implementation("androidx.coordinatorlayout:coordinatorlayout:1.2.0")
    implementation("androidx.constraintlayout:constraintlayout:2.1.4")
    // Material 3 — provides MaterialSwitch, Slider, TabLayout, AppBarLayout,
    // MaterialCardView. We never mix in legacy SwitchMaterial.
    implementation("com.google.android.material:material:1.12.0")
}
