plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// versionName is the workspace version (Cargo.toml [workspace.package]; the tag of a release is v + it, release.yml).
val workspaceVersion: String = Regex("""(?m)^version\s*=\s*"([^"]+)"""")
    .find(rootProject.file("../Cargo.toml").readText().substringAfter("[workspace.package]"))
    ?.groupValues?.get(1)
    ?: error("Cargo.toml has no [workspace.package] version")

// versionCode must grow with every build or Android refuses to update the installed app. The workflows pass the commit time of
// the built commit in minutes (git log -1 --format=%ct, divided by 60): deterministic for a commit, increasing along master,
// the same for the android and release workflows, and below the 2.1 billion limit until the year 5000. A local build without
// it is 1 (installable once; pass -PkobVersionCode=<n> or KOB_VERSION_CODE to update an earlier install).
val kobVersionCode: Int = (providers.gradleProperty("kobVersionCode").orNull ?: providers.environmentVariable("KOB_VERSION_CODE").orNull)
    ?.toIntOrNull()?.takeIf { it in 1..2_100_000_000 }
    ?: 1

// The release key, from the environment (the workflows decode the repository secrets into these; docs/ops/executor-quickstart.md 9).
// All four set: the release build is signed with it, and a newer APK installs over an older one and keeps its data (the operator key).
// Otherwise it falls back to the debug key, which is different on every machine and every CI run: such an APK cannot update one
// signed with another key (the workflow names it "debug-signed" and warns).
// a secret pasted with a trailing newline (gh secret set from a pipe on Windows adds one) must still match the keystore
fun env(name: String): String? = providers.environmentVariable(name).orNull?.trim { it.isWhitespace() }?.takeIf { it.isNotEmpty() }
val releaseKeystore: String? = env("KOB_KEYSTORE_FILE")
val releaseKeystorePassword: String? = env("KOB_KEYSTORE_PASSWORD")
val releaseKeyAlias: String? = env("KOB_KEY_ALIAS")
val releaseKeyPassword: String? = env("KOB_KEY_PASSWORD")
val hasReleaseKey = listOf(releaseKeystore, releaseKeystorePassword, releaseKeyAlias, releaseKeyPassword).all { it != null }

android {
    namespace = "org.kob.executor"
    compileSdk = 36

    defaultConfig {
        applicationId = "org.kob.executor"
        minSdk = 24
        targetSdk = 36
        versionCode = kobVersionCode
        versionName = workspaceVersion
        // the executor is built for arm64 only (aarch64-linux-android)
        ndk { abiFilters += "arm64-v8a" }
    }

    signingConfigs {
        if (hasReleaseKey) {
            create("kobRelease") {
                storeFile = file(releaseKeystore!!)
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // never debuggable: no `run-as org.kob.executor` to read the operator key off the phone
            isDebuggable = false
            signingConfig = if (hasReleaseKey) signingConfigs.getByName("kobRelease") else signingConfigs.getByName("debug")
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
