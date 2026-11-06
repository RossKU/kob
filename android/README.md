# KOB Executor for Android

The testnet-10 Android app: `kobd` (the Executor or the Indexer) in a foreground service, with a setup and status screen.
How to install and use it: `docs/ops/executor-quickstart.md`, section 9.

## Layout

| File | What it does |
|---|---|
| `app/src/main/kotlin/org/kob/executor/MainActivity.kt` | Setup (node, role), funding address and balance, run switch, status and log |
| `.../ExecutorService.kt` | Foreground service: runs `kobd` with no arguments, keeps a wake lock, restarts it after an exit (5 s, doubling to 5 min) |
| `.../Kob.kt` | Where the binary and the files are; runs `kobd` subcommands |
| `.../WithdrawActivity.kt` | Stops the executor, then `kobd withdraw --to ... --amount ... --yes` |
| `.../ExportKeyActivity.kt` | Shows, copies or saves the operator key (no screenshots) |
| `app/src/main/AndroidManifest.xml` | Permissions, the service (`specialUse` on Android 15+, `dataSync` on 10 to 14), no backup |

The app is plain framework views (no AndroidX, no layout files). Its files are in the app's private storage, `files/kob`:
`kob-executor.toml`, `kob-operator.key` and `kob-data/`. The app talks to `kobd` through:

* `kobd setup --non-interactive --node resolver|<url> --executor|--indexer --dir <files/kob> --new-key`: writes the
  configuration and, for the Executor, the key; prints one JSON object (`address`, `roles`, ...).
* `kobd` with no arguments and `KOB_EXECUTOR_DIR=<files/kob>`: runs the configuration. The app reads its stdout for the log
  and the status line, and `http://127.0.0.1:8090/v1/health` for the status screen.
* `kobd address-balance --dir <files/kob>`: the funding address and its balance (one JSON object).
* `kobd withdraw --to <address> --amount <KAS|all> --yes --dir <files/kob>`.

## Build

The **android** workflow (`.github/workflows/android.yml`) builds the APK on every push to `master` that touches `android/`,
`crates/kob-executor/` or the workflow, on a manual run, and for a release (`release.yml`). It:

1. builds `kobd` for `aarch64-linux-android` with the runner's NDK (clang for API 24, 16 KB page alignment):
   `cargo build --release --locked --target aarch64-linux-android -p kob-executor --features kob-executor/deploy-tn10`;
2. copies it to `app/src/main/jniLibs/arm64-v8a/libkobexecutor.so` (never committed): Android runs it from the app's
   native library folder;
3. runs `./gradlew assembleRelease` with JDK 17 and Android SDK platform 36, and checks the APK (the binary is in it, the
   signature verifies, `versionName` / `versionCode`, not debuggable).

A local build needs the same: JDK 17, the Android SDK (platform 36, build tools 36.0.0) and, for the binary, the NDK and the
Rust target `aarch64-linux-android`. Without the binary the APK still builds, and the app says it has no executor binary.

| | Value |
|---|---|
| `minSdk` / `targetSdk` / `compileSdk` | 24 (Android 7.0) / 36 / 36 |
| ABI | `arm64-v8a` only |
| Gradle / Android Gradle Plugin / Kotlin | 8.14.3 / 8.11.1 / 2.2.0 |
| `versionName` | the workspace version (`Cargo.toml`) |
| `versionCode` | the commit time in minutes (`git log -1 --format=%ct` / 60, set by the workflows); 1 in a local build unless `KOB_VERSION_CODE` or `-PkobVersionCode` is set |

The app has no unit tests.

## Release signing (for the maintainer)

Android updates an installed app only with an APK signed by the same key. The release key is made once, outside the
repository, and stored as four repository secrets that `android.yml` signs with (for pushes and, through `release.yml`,
releases):

1. Run `node scripts/make-android-keystore.mjs` (needs a JDK's `keytool`). It writes the keystore, a random password file and
   the keystore as base64 to `~/.kob/android-signing` (`--out <folder>` for another one; a folder inside the repository is
   refused), prints the signer certificate's SHA-256 and the next steps, and prints no password.
2. Store the secrets:

   ```
   # bash
   gh secret set ANDROID_KEYSTORE_B64      --repo RossKU/kob < ~/.kob/android-signing/kob-release.jks.b64.txt
   gh secret set ANDROID_KEYSTORE_PASSWORD --repo RossKU/kob < ~/.kob/android-signing/kob-release.password.txt
   gh secret set ANDROID_KEY_PASSWORD      --repo RossKU/kob < ~/.kob/android-signing/kob-release.password.txt
   gh secret set ANDROID_KEY_ALIAS         --repo RossKU/kob --body kob-release
   # PowerShell
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.jks.b64.txt  | gh secret set ANDROID_KEYSTORE_B64      --repo RossKU/kob
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.password.txt | gh secret set ANDROID_KEYSTORE_PASSWORD --repo RossKU/kob
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.password.txt | gh secret set ANDROID_KEY_PASSWORD      --repo RossKU/kob
   gh secret set ANDROID_KEY_ALIAS --repo RossKU/kob --body kob-release
   ```

   (or in the browser: Settings, Secrets and variables, Actions). `node scripts/make-android-keystore.mjs --steps` prints
   the steps again.
3. Run the **android** workflow: it must not warn "Android APK is debug-signed", and the signer SHA-256 in its summary must be
   the one the script printed.
4. **Back up the folder.** A lost keystore means installed apps can never be updated in place. Never commit it (`*.jks`,
   `*.keystore`, `*.p12` are ignored) and never paste the password anywhere.

Without the secrets the build signs with a throwaway debug key, names the file `...-debug-signed.apk` and warns (in a release,
also in the draft's notes). Either way the APK is not debuggable, so `adb run-as` cannot read the key off a phone.
