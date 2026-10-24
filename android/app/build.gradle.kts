plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.kob.executor"
    compileSdk = 36

    defaultConfig {
        applicationId = "org.kob.executor"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-testnet-10"
        // the executor is built for arm64 only (aarch64-linux-android)
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    packaging {
        // the executor is a program, not a library: it must be extracted to the app's nativeLibraryDir to be executable
        // (extractNativeLibs=true), not mapped out of the APK
        jniLibs.useLegacyPackaging = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        // a sideloaded app: no Play Store checks
        abortOnError = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}
