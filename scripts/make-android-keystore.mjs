#!/usr/bin/env node
// Creates the release signing key of the Android app (once, by the maintainer) and prints the steps that store it as the GitHub
// repository secrets android.yml / release.yml sign with.
//
//   node scripts/make-android-keystore.mjs [--out <dir>] [--alias kob-release] [--repo RossKU/kob] [--keytool <path>]
//   node scripts/make-android-keystore.mjs --steps [--out <dir>]     print the steps again for an existing keystore
//
// Why: Android installs an update only over an app signed with the SAME key. With this key in the secrets, every release and
// every CI build of the app carries one signature, so a new APK installs over the old one and keeps the app's data (the operator
// key and its funds). Without it, the APK is signed with a throwaway debug key and cannot be updated in place.
//
// What it writes (default folder: ~/.kob/android-signing, outside every repository; it refuses a folder inside this one):
//   kob-release.jks             the keystore (PKCS12, RSA 4096, valid 30 years)
//   kob-release.password.txt    its password, which is also the key's (random; no trailing newline)
//   kob-release.jks.b64.txt     the keystore as one line of base64, for the secret ANDROID_KEYSTORE_B64
// It never prints a password, and never commits anything (*.jks, *.keystore, *.p12 are in .gitignore). BACK THE FOLDER UP (a
// password manager, an offline copy): if the keystore is lost, installed apps can only be replaced by uninstalling them, which
// deletes their operator keys. Keep it private: anyone holding it can sign an APK that updates yours.
//
// Needs a JDK's `keytool` (on PATH, in JAVA_HOME, or Android Studio's bundled one) and Node >= 18.
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FLAGS = new Set(['steps', 'help']);

function parseArgs(argv) {
  const o = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith('--')) fail(`unexpected argument ${a}`);
    const k = a.slice(2);
    if (FLAGS.has(k)) o[k] = true;
    else if (argv[i + 1] === undefined) fail(`${a} needs a value`);
    else o[k] = argv[++i];
  }
  return o;
}

function fail(msg) {
  console.error(`make-android-keystore: ${msg}`);
  process.exit(2);
}

const exe = process.platform === 'win32' ? 'keytool.exe' : 'keytool';

function findKeytool(explicit) {
  const candidates = [];
  if (explicit) candidates.push(explicit);
  if (process.env.JAVA_HOME) candidates.push(path.join(process.env.JAVA_HOME, 'bin', exe));
  candidates.push(exe); // PATH
  const roots =
    process.platform === 'win32'
      ? [
          'C:/Program Files/Android/Android Studio/jbr/bin',
          'C:/Program Files/Android/Android Studio1/jbr/bin',
          ...['Eclipse Adoptium', 'Microsoft', 'Java', 'Zulu', 'Amazon Corretto'].flatMap((v) => {
            const base = path.join('C:/Program Files', v);
            return fs.existsSync(base) ? fs.readdirSync(base).map((d) => path.join(base, d, 'bin')) : [];
          }),
        ]
      : ['/Applications/Android Studio.app/Contents/jbr/Contents/Home/bin', '/usr/lib/jvm/default-java/bin', '/opt/android-studio/jbr/bin'];
  for (const r of roots) candidates.push(path.join(r, exe));
  for (const c of candidates) {
    try {
      execFileSync(c, ['-help'], { stdio: 'ignore' });
      return c;
    } catch (e) {
      if (e.code !== 'ENOENT' && e.status !== undefined && e.status !== null) return c; // it ran (some JDKs exit non-zero on -help)
    }
  }
  return null;
}

/** True when `p` is `dir` or inside it. */
const inside = (dir, p) => {
  const r = path.relative(dir, p);
  return r === '' || (!r.startsWith('..') && !path.isAbsolute(r));
};

function restrict(file) {
  // best effort: owner-only (POSIX modes are set at creation; on Windows the user profile is already private, this tightens it)
  if (process.platform !== 'win32') return;
  try {
    execFileSync('icacls', [file, '/inheritance:r', '/grant:r', `${os.userInfo().username}:F`], { stdio: 'ignore' });
  } catch {
    /* the profile folder's own ACL still applies */
  }
}

function printSteps({ dir, jks, pass, b64, alias, repo, sha256 }) {
  const q = (p) => `"${p}"`;
  console.log(`
Steps to store the secrets (do this once; the values are read from the files, so no password is typed or shown):

A. With the GitHub CLI (gh auth login, as an admin of ${repo}):

   bash / Git Bash / zsh:
     gh secret set ANDROID_KEYSTORE_B64      --repo ${repo} < ${q(b64)}
     gh secret set ANDROID_KEYSTORE_PASSWORD --repo ${repo} < ${q(pass)}
     gh secret set ANDROID_KEY_PASSWORD      --repo ${repo} < ${q(pass)}
     gh secret set ANDROID_KEY_ALIAS         --repo ${repo} --body ${alias}

   PowerShell:
     Get-Content -Raw ${q(b64)}  | gh secret set ANDROID_KEYSTORE_B64      --repo ${repo}
     Get-Content -Raw ${q(pass)} | gh secret set ANDROID_KEYSTORE_PASSWORD --repo ${repo}
     Get-Content -Raw ${q(pass)} | gh secret set ANDROID_KEY_PASSWORD      --repo ${repo}
     gh secret set ANDROID_KEY_ALIAS --repo ${repo} --body ${alias}

B. Or in the browser: github.com/${repo} -> Settings -> Secrets and variables -> Actions -> "New repository secret", four times:
     ANDROID_KEYSTORE_B64       the whole content of ${b64}   (open the file, select all, copy; one line)
     ANDROID_KEYSTORE_PASSWORD  the whole content of ${pass}
     ANDROID_KEY_PASSWORD       the same content again
     ANDROID_KEY_ALIAS          ${alias}

C. Check: github.com/${repo}/settings/secrets/actions lists the four names. Then run the android workflow (Actions -> android -> Run
   workflow) or push a release tag: the run must NOT warn "Android APK is debug-signed", and its summary shows the signer
   certificate SHA-256, which must be:
     ${sha256}

D. Back up ${dir} (password manager, an offline copy), and keep it out of every repository and chat.

From the first APK built with these secrets on, every later build installs over it and keeps the app's data. An APK built BEFORE
this (debug-signed) cannot be updated in place: in that app tap "Withdraw funds" to send the money out (and "Export the key" if
you want a copy), then uninstall it and install the new APK.`);
}

const o = parseArgs(process.argv.slice(2));
if (o.help) {
  console.log(fs.readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').filter((l) => l.startsWith('//')).slice(0, 24).map((l) => l.slice(3)).join('\n'));
  process.exit(0);
}
const dir = path.resolve(o.out ?? path.join(os.homedir(), '.kob', 'android-signing'));
const alias = o.alias ?? 'kob-release';
const repo = o.repo ?? 'RossKU/kob';
if (!/^[A-Za-z0-9._-]+$/.test(alias)) fail('--alias may hold letters, digits, dot, dash and underscore only');
if (inside(repoRoot, dir)) fail(`${dir} is inside the repository: choose a folder outside it (default ~/.kob/android-signing)`);

const jks = path.join(dir, 'kob-release.jks');
const pass = path.join(dir, 'kob-release.password.txt');
const b64 = path.join(dir, 'kob-release.jks.b64.txt');

const keytool = findKeytool(o.keytool);
if (!keytool) fail('keytool not found: install a JDK 17 (or Android Studio) and put its bin folder on PATH, or pass --keytool <path>');

/** The certificate's SHA-256 fingerprint (public), read with the password from the file through the environment. */
function fingerprint() {
  const text = execFileSync(keytool, ['-list', '-v', '-keystore', jks, '-alias', alias, '-storepass:env', 'KOB_KS_PASS'], {
    env: { ...process.env, KOB_KS_PASS: fs.readFileSync(pass, 'utf8') },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const m = /SHA-?256:\s*([0-9A-F:]{95})/i.exec(text);
  if (!m) fail('could not read the certificate fingerprint');
  return m[1];
}

if (o.steps) {
  for (const f of [jks, pass, b64]) if (!fs.existsSync(f)) fail(`${f} does not exist: run without --steps to create the keystore`);
  printSteps({ dir, jks, pass, b64, alias, repo, sha256: fingerprint() });
  process.exit(0);
}

for (const f of [jks, pass, b64]) {
  if (fs.existsSync(f)) {
    fail(`${f} already exists and is not overwritten (replacing the key would stop every installed app from updating). ` +
      'Use --steps to print the steps again, or --out <another folder> for a different key on purpose.');
  }
}
fs.mkdirSync(dir, { recursive: true, mode: 0o700 });

const password = crypto.randomBytes(24).toString('base64url'); // 192 bits, URL-safe characters only
// the password reaches keytool through the environment, never the command line (which other accounts can list)
const env = { ...process.env, KOB_KS_PASS: password };
try {
  fs.writeFileSync(pass, password, { flag: 'wx', mode: 0o600 });
  execFileSync(
    keytool,
    [
      '-genkeypair',
      '-keystore', jks,
      '-storetype', 'PKCS12',
      '-alias', alias,
      '-keyalg', 'RSA',
      '-keysize', '4096',
      '-validity', '10950',
      '-dname', 'CN=KOB Executor Android release, O=KOB',
      '-storepass:env', 'KOB_KS_PASS',
      '-keypass:env', 'KOB_KS_PASS',
    ],
    { env, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  fs.writeFileSync(b64, fs.readFileSync(jks).toString('base64'), { flag: 'wx', mode: 0o600 });
} catch (e) {
  // leave nothing half-made behind: a half keystore would block the next run
  for (const f of [jks, pass, b64]) fs.rmSync(f, { force: true });
  const why = e.stderr ? String(e.stderr).trim() : e.message;
  fail(`keytool failed: ${why}`);
}
for (const f of [jks, pass, b64]) restrict(f);

const sha256 = fingerprint();
console.log(`Created the Android release key in ${dir}:
  ${jks}
  ${pass}   (the password; not shown)
  ${b64}
Signer certificate SHA-256: ${sha256}`);
printSteps({ dir, jks, pass, b64, alias, repo, sha256 });
