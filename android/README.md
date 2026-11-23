# KOB Executor for Android

The testnet-10 Android app: `kobd` (the Executor or the Indexer) in a foreground service, with a setup and status screen.
How to install and use it: `docs/ops/executor-quickstart.md`, section 9.

## Layout

| File | What it does |
|---|---|
| `app/src/main/kotlin/org/kob/executor/MainActivity.kt` | Setup (node, role), funding address and balance, run switch, status and log |
| `.../ExecutorService.kt` | Foreground service: updates the database (`kobd db migrate`), then runs `kobd` with no arguments, keeps a wake lock, restarts it after an exit (5 s, doubling to 5 min) |
| `.../Kob.kt` | Where the binary and the files are; runs `kobd` subcommands |
| `.../WithdrawActivity.kt` | Stops the executor, then `kobd withdraw --to ... --amount ... --yes` |
| `.../ExportKeyActivity.kt` | Shows, copies or saves the operator key (no screenshots) |
| `app/src/main/AndroidManifest.xml` | Permissions, the service (`specialUse` on Android 15+, `dataSync` on 10 to 14), no backup |

The app is plain framework views (no AndroidX, no layout files). Its files are in the app's private storage, `files/kob`:
`kobd.toml`, `kob-operator.key` and `kob-data/`. The app talks to `kobd` through:

* `kobd setup --non-interactive --node resolver|<url> --executor|--indexer --dir <files/kob> --new-key`: writes the
  configuration and, for the Executor, the key; prints one JSON object (`address`, `roles`, ...).
* `kobd db --config <files/kob>/kobd.toml migrate`, before each start when `kob-data/index.sqlite3` exists: brings
  a store of an earlier build to the current schema (a moment on a current one). If it fails, the service stops and shows why.
  The previous store it keeps (`kob-data/index.sqlite3.before-migrate`, with its -wal / -shm) is deleted by the service once
  a `kobd` started after it has reported, on `/v1/health`, the indexer following the chain on the full store (`state`
  `following` or `catching_up`, `book_complete` not false) and that same process has run for 10 minutes; an exit, a restart
  or a stop before that keeps the file, and the next start watches again.
* `kobd` with no arguments and `KOBD_DIR=<files/kob>`: runs the configuration. The app reads its stdout for the log
  and the status line, and `http://127.0.0.1:8090/v1/health` for the status screen.
* `kobd address-balance --dir <files/kob>`: the funding address and its balance (one JSON object).
* `kobd withdraw --to <address> --amount <KAS|all> --yes --dir <files/kob>`.

## Build

The **android** workflow (`.github/workflows/android.yml`) builds the APK on every push to `master` that touches `android/`,
`crates/kobd/` or the workflow, on a manual run, and for a release (`release.yml`). It:

1. builds `kobd` for `aarch64-linux-android` with the runner's NDK (clang for API 24, 16 KB page alignment):
   `cargo build --release --locked --target aarch64-linux-android -p kobd --features kobd/deploy-tn10`;
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
repository, and stored as four secrets of the GitHub **environment** `android-release`, which the signing job of `android.yml`
runs in on `master` and on `v*` tags (for pushes and, through `release.yml`, releases). They must not be repository secrets:
GitHub gives those to every workflow of every branch, so anyone who can push a branch could sign an APK that updates the operator
app over its key. `release.yml` fails a tag run while a repository copy exists, and a push run warns.

0. **The environment.** Repository *Settings* > *Environments* > *New environment*, name `android-release` > *Configure
   environment*. Tick **Required reviewers**, add yourself, *Save protection rules*; untick *Allow administrators to bypass
   configured protection rules* if shown. Under *Deployment branches and tags* choose **Selected branches and tags** and add two
   rules: *Ref type: Branch* `master`, and *Ref type: Tag* `v*`. With a reviewer, every signing run (each `master` push that
   touches the app, and each release) waits for your approval under *Review deployments*.
1. Run `node scripts/make-android-keystore.mjs` (needs a JDK's `keytool`). It writes the keystore, a random password file and
   the keystore as base64 to `~/.kob/android-signing` (`--out <folder>` for another one; a folder inside the repository is
   refused), prints the signer certificate's SHA-256 and the next steps, and prints no password.
2. Store the secrets:

   ```
   # bash
   gh secret set ANDROID_KEYSTORE_B64      --env android-release --repo RossKU/kob < ~/.kob/android-signing/kob-release.jks.b64.txt
   gh secret set ANDROID_KEYSTORE_PASSWORD --env android-release --repo RossKU/kob < ~/.kob/android-signing/kob-release.password.txt
   gh secret set ANDROID_KEY_PASSWORD      --env android-release --repo RossKU/kob < ~/.kob/android-signing/kob-release.password.txt
   gh secret set ANDROID_KEY_ALIAS         --env android-release --repo RossKU/kob --body kob-release
   # PowerShell
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.jks.b64.txt  | gh secret set ANDROID_KEYSTORE_B64      --env android-release --repo RossKU/kob
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.password.txt | gh secret set ANDROID_KEYSTORE_PASSWORD --env android-release --repo RossKU/kob
   Get-Content -Raw $HOME\.kob\android-signing\kob-release.password.txt | gh secret set ANDROID_KEY_PASSWORD      --env android-release --repo RossKU/kob
   gh secret set ANDROID_KEY_ALIAS --env android-release --repo RossKU/kob --body kob-release
   ```

   (or in the browser: *Settings* > *Environments* > `android-release` > *Environment secrets* > *Add environment secret*).
   `node scripts/make-android-keystore.mjs --steps` prints the steps again.
2a. **Delete the repository copies** if they exist: *Settings* > *Secrets and variables* > *Actions* > *Repository secrets*:
   delete `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD` there (only
   after the environment has all four).
3. Run the **android** workflow on `master`: it must not warn "Android APK is debug-signed", and the signer SHA-256 in its summary must be
   the one the script printed.
4. **Back up the folder.** A lost keystore means installed apps can never be updated in place. Never commit it (`*.jks`,
   `*.keystore`, `*.p12` are ignored) and never paste the password anywhere.

Without the secrets the build signs with a throwaway debug key, names the file `...-debug-signed.apk` and warns (in a release,
also in the draft's notes). Either way the APK is not debuggable, so `adb run-as` cannot read the key off a phone.
