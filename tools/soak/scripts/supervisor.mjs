// Soak supervisor: starts the miner, the two executors, the bots and the checker; restarts any that exits (exponential backoff);
// writes every child's output to a size-rotated log; samples memory / CPU of every child once a minute; stops everything when
// run/STOP appears (scripts/stop.ps1) or on SIGINT / SIGTERM. run/RESTART-<child> (scripts/restart-child.ps1) restarts one child only,
// with its spec rebuilt from a fresh read of the config (e.g. the miner after a binary swap or a `miner` block edit).
//
//   node scripts/supervisor.mjs [--config run/config.json] [--only miner,exec-a,...]
import { spawn, execFile } from 'node:child_process';
import { appendFileSync, createWriteStream, existsSync, mkdirSync, readFileSync, renameSync, rmSync, statSync, statfsSync, writeFileSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { minerSpec } from './miner-config.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const SOAK = resolve(HERE, '..');
const argv = process.argv.slice(2);
const opt = (n, d) => (argv.includes(n) ? argv[argv.indexOf(n) + 1] : d);
const configPath = resolve(opt('--config', join(SOAK, 'run', 'config.json')));
let cfg = JSON.parse(readFileSync(configPath, 'utf8'));
const RUN = isAbsolute(cfg.runDir) ? cfg.runDir : resolve(dirname(configPath), cfg.runDir);
const abs = (p) => (isAbsolute(p) ? p : resolve(dirname(configPath), p));
const LOGS = join(RUN, 'logs');
const STATS = join(RUN, 'stats');
mkdirSync(LOGS, { recursive: true });
mkdirSync(STATS, { recursive: true });
const keys = JSON.parse(readFileSync(join(RUN, 'keys.json'), 'utf8'));

const LOG_MAX = Number(process.env.SOAK_LOG_MAX_BYTES ?? 20 * 1024 * 1024);
const LOG_KEEP = 5;
const STOP_FILE = join(RUN, 'STOP');
// disk guard (free space of the run directory's drive, checked every minute): below DISK_WARN a warning (and stats/disk.json); below
// DISK_LOW the low-disk mode: every log rotates now, rotated logs are gzipped, and the bots' / checker's info lines are dropped (only
// warnings and errors are written) until the drive has DISK_LOW + 1 GB free again
const GB = 1024 ** 3;
const DISK_WARN = Number(process.env.SOAK_DISK_WARN_GB ?? 10) * GB;
const DISK_LOW = Number(process.env.SOAK_DISK_LOW_GB ?? 3) * GB;
let lowDisk = false;
rmSync(STOP_FILE, { force: true });

const say = (msg, f = {}) => {
  const line = JSON.stringify({ t: new Date().toISOString(), src: 'supervisor', msg, ...f });
  process.stdout.write(line + '\n');
  appendFileSync(join(LOGS, 'supervisor.log'), line + '\n');
};

/** gzips the rotated copies of a log (name.log.N -> name.log.N.gz) */
function gzipRotated(path) {
  for (let i = 1; i <= LOG_KEEP; i++) {
    const p = `${path}.${i}`;
    if (!existsSync(p)) continue;
    try {
      writeFileSync(`${p}.gz`, gzipSync(readFileSync(p)));
      rmSync(p);
    } catch (e) {
      say('gzip failed', { path: p, error: String(e) });
    }
  }
}

/** size-rotated log sink: name.log -> name.log.1 ... name.log.5 */
class RotatingLog {
  constructor(name) {
    this.path = join(LOGS, `${name}.log`);
    this.open();
  }
  open() {
    this.size = existsSync(this.path) ? statSync(this.path).size : 0;
    this.stream = createWriteStream(this.path, { flags: 'a' });
  }
  write(chunk) {
    if (lowDisk && this.quiet) {
      // low-disk mode: only the warnings and errors of a chatty child
      const keep = String(chunk).split('\n').filter((l) => l && /"lvl":"(warn|error)"|\b(WARN|ERROR)\b|Error|panicked/.test(l));
      if (!keep.length) return;
      chunk = keep.join('\n') + '\n';
    }
    this.stream.write(chunk);
    this.size += chunk.length;
    if (this.size > LOG_MAX) this.rotate();
  }
  rotate() {
    this.stream.end();
    for (let i = LOG_KEEP - 1; i >= 1; i--) {
      if (existsSync(`${this.path}.${i}`)) renameSync(`${this.path}.${i}`, `${this.path}.${i + 1}`);
    }
    if (existsSync(this.path)) renameSync(this.path, `${this.path}.1`);
    this.open();
    if (lowDisk) gzipRotated(this.path);
  }
}

function x402Config(ex) {
  const merchant = keys[cfg.x402.merchantKey];
  const p = join(RUN, ex.name, 'x402.json');
  const c = {
    network: 'kaspa:testnet-10',
    node: cfg.nodeUrl,
    listen: ex.x402,
    ledger: join(RUN, ex.name, 'x402-ledger.jsonl'),
    auth: 'required',
    confirmationsDaa: 100,
    maxFeeSompi: 50000000,
    minAmountSompi: 1000,
    swap: true,
    registry: join(RUN, 'registry', 'tokens.json'),
    allowIssuerControlled: false,
    merchants: [
      {
        id: 'soak-shop',
        apiKeySha256: readFileSync(join(RUN, 'x402-merchant.sha256'), 'utf8').trim(),
        allowedPayTo: [merchant.address],
        allowedAssets: ['KAS', JSON.parse(readFileSync(join(RUN, 'state.json'), 'utf8')).token.covenantId],
        rateLimit: { burst: 20, perSecond: 5 },
      },
    ],
    rateLimit: { perIp: { burst: 30, perSecond: 10 }, perMerchant: { burst: 60, perSecond: 20 } },
    maxBodyBytes: 1048576,
    bodyDeadlineMs: 10000,
    headerTimeoutMs: 10000,
    settleWaitMs: 30000,
    pollIntervalMs: 200,
    nodeTimeoutMs: 8000,
    reorgWatchDaa: 36000,
    reconcileIntervalSeconds: 30,
    maxConcurrentSettles: 64,
    maxConnections: 1024,
    killSwitchFile: join(RUN, ex.name, 'x402.kill'),
  };
  if (cfg.x402.invoiceIntent?.enabled) {
    // docs/ops/executor.md A.7: the keeper key only receives what token intents leave over (the executor's own hot key)
    c.intents = { enabled: true, keeperPubkey: keys[ex.key].publicKey, fillerSompi: 20000000, maxAttempts: 20, maxBuilds: 24, maxCandidates: 8, lockMarginDaa: 10 };
    c.invoices = { enabled: true, store: join(RUN, ex.name, 'x402-invoices.jsonl'), maxLifetimeSeconds: 3600, maxOpenPerMerchant: 1000 };
  }
  writeFileSync(p, JSON.stringify(c, null, 2));
  return p;
}

function specs() {
  const node = process.execPath;
  const soak = join(SOAK, 'dist', 'soak.mjs');
  // the miner: settings file run/miner.json from config.miner (miner-config.mjs; the miner re-reads it on change)
  const out = [minerSpec(cfg, { run: RUN, minerBin: abs(cfg.minerBin), bankAddress: keys[cfg.bank.key].address })];
  for (const ex of cfg.executors) {
    const dir = join(RUN, ex.name);
    mkdirSync(join(dir, 'data'), { recursive: true });
    const args = [
      'run',
      '--network', 'testnet-10',
      '--rpc-url', cfg.nodeUrl,
      '--data-dir', join(dir, 'data'),
      '--tokens', join(RUN, 'registry', 'tokens.json'),
      '--start', 'sink',
      '--listen', ex.api,
      '--key-file', join(dir, 'operator.key'),
      '--metrics-file', join(dir, 'metrics.prom'),
      '--pause-file', join(dir, 'pause'),
      ...(ex.args ?? []),
    ];
    // every client of the soak (bots, checker, UI) is 127.0.0.1: lift the per-client rate limit; the UI calls the API cross-origin.
    // The file is rewritten on every start, so a VSPC request timeout other than the executor's default (180 s) must come from the
    // config (`rpcTimeoutSecs`, per executor or global): an edit of executor.toml is lost on the next start.
    const rpcTimeout = ex.rpcTimeoutSecs ?? cfg.rpcTimeoutSecs;
    // parallel window fetch while catching up (docs/ops/executor.md, "Parallel fetch"): connections and the memory budget of the windows held ahead
    const fetchParallel = ex.fetchParallel ?? cfg.fetchParallel;
    const prefetchMaxMb = ex.prefetchMaxMb ?? cfg.prefetchMaxMb;
    // parallel windows up to this lag (blue score) behind the sink; the executor's default is 1,200
    const prefetchMinLagBlue = ex.prefetchMinLagBlue ?? cfg.prefetchMinLagBlue;
    // lag tolerance of the executor's planning gates, in seconds (the bots' gate reads the same `maxLagSecs`; 0 = strict)
    const maxLagSecs = ex.maxLagSecs ?? cfg.maxLagSecs;
    writeFileSync(
      join(dir, 'executor.toml'),
      [
        ...(rpcTimeout ? [`rpc_timeout_secs = ${Number(rpcTimeout)}`] : []),
        ...(fetchParallel ? [`fetch_parallel = ${Number(fetchParallel)}`] : []),
        ...(prefetchMaxMb ? [`prefetch_max_mb = ${Number(prefetchMaxMb)}`] : []),
        ...(prefetchMinLagBlue !== undefined ? [`prefetch_min_lag_blue = ${Number(prefetchMinLagBlue)}`] : []),
        ...(maxLagSecs !== undefined ? [`max_lag_secs = ${Number(maxLagSecs)}`] : []),
        '[api]',
        'cors_allow_origin = "*"',
        'max_ws_per_ip = 100',
        '[api.rate_limit]',
        'per_ip_rps = 500.0',
        'per_ip_burst = 1000',
        'global_rps = 2000.0',
        'global_burst = 4000',
        '',
      ].join('\n'),
    );
    args.push('--config', join(dir, 'executor.toml'));
    if (ex.x402 && cfg.x402?.enabled) args.push('--x402-config', x402Config(ex));
    out.push({ name: ex.name, cmd: abs(cfg.executorBin), args, env: { RUST_LOG: process.env.SOAK_RUST_LOG ?? 'info', NO_COLOR: '1' } });
  }
  out.push({ name: 'bots', cmd: node, args: ['--max-old-space-size=768', soak, 'bots', '--config', configPath], env: {} });
  out.push({ name: 'checker', cmd: node, args: ['--max-old-space-size=512', soak, 'checker', '--config', configPath], env: {} });
  if (cfg.ui?.listen) out.push({ name: 'ui', cmd: node, args: [join(HERE, 'serve-ui.mjs'), '--config', configPath], env: {} });
  const only = opt('--only');
  return only ? out.filter((s) => only.split(',').includes(s.name)) : out;
}

const children = new Map();
let stopping = false;

/** run/RESTART-<name>: kill that child; the exit handler starts it again with a spec rebuilt from a fresh read of the config */
function restartRequests() {
  for (const st of children.values()) {
    const f = join(RUN, `RESTART-${st.spec.name}`);
    if (!existsSync(f)) continue;
    rmSync(f, { force: true });
    try {
      cfg = JSON.parse(readFileSync(configPath, 'utf8'));
      const fresh = specs().find((s) => s.name === st.spec.name);
      if (fresh) st.spec = fresh;
    } catch (e) {
      say('restart: config re-read failed, keeping the old spec', { name: st.spec.name, error: String(e) });
    }
    say('restart requested', { name: st.spec.name });
    st.backoffMs = 2000;
    if (st.child) execFile('taskkill', ['/PID', String(st.pid), '/T', '/F'], { windowsHide: true }, () => {});
    else start(st.spec);
  }
}

function start(spec) {
  const st = children.get(spec.name) ?? { spec, restarts: 0, backoffMs: 2000, log: new RotatingLog(spec.name), exits: [] };
  st.log.quiet = spec.name === 'bots' || spec.name === 'checker';
  children.set(spec.name, st);
  const child = spawn(spec.cmd, spec.args, { env: { ...process.env, ...spec.env }, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  st.child = child;
  st.pid = child.pid;
  st.startedAt = Date.now();
  say('started', { name: spec.name, pid: child.pid });
  child.stdout.on('data', (d) => st.log.write(d));
  child.stderr.on('data', (d) => st.log.write(d));
  child.on('error', (e) => say('spawn error', { name: spec.name, error: String(e) }));
  child.on('exit', (code, signal) => {
    const up = Date.now() - st.startedAt;
    st.exits.push({ at: Date.now(), code, signal, upMs: up });
    st.exits = st.exits.slice(-20);
    st.child = null;
    say('exited', { name: spec.name, code, signal, upSec: Math.round(up / 1000) });
    if (stopping) return;
    if (up > 10 * 60_000) st.backoffMs = 2000;
    const wait = st.backoffMs;
    st.backoffMs = Math.min(st.backoffMs * 2, 60_000);
    st.restarts += 1;
    setTimeout(() => !stopping && !st.child && start(st.spec), wait);
  });
}

function writeStatus() {
  const s = {
    ts: Date.now(),
    pid: process.pid,
    configPath,
    children: [...children.values()].map((c) => ({
      name: c.spec.name,
      pid: c.child ? c.pid : null,
      running: !!c.child,
      startedAt: c.startedAt,
      restarts: c.restarts,
      lastExits: c.exits.slice(-5),
    })),
  };
  writeFileSync(join(RUN, 'supervisor.json'), JSON.stringify(s, null, 2));
}

/** one PowerShell call per minute: working set and CPU seconds of every child */
function sampleResources() {
  const pids = [...children.values()].filter((c) => c.child).map((c) => c.pid);
  if (!pids.length) return;
  const ps = `Get-Process -Id ${pids.join(',')} -ErrorAction SilentlyContinue | Select-Object Id,WorkingSet64,PrivateMemorySize64,CPU | ConvertTo-Json -Compress`;
  execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', ps], { windowsHide: true, timeout: 30_000 }, (err, stdout) => {
    if (err || !stdout.trim()) return;
    let rows;
    try {
      rows = JSON.parse(stdout);
    } catch {
      return;
    }
    rows = Array.isArray(rows) ? rows : [rows];
    const byPid = new Map(rows.map((r) => [r.Id, r]));
    const sample = { ts: Date.now(), procs: {} };
    let total = 0;
    for (const c of children.values()) {
      const r = c.child && byPid.get(c.pid);
      if (!r) continue;
      const mb = Math.round(r.WorkingSet64 / 1e6);
      total += mb;
      sample.procs[c.spec.name] = { pid: c.pid, wsMb: mb, privMb: Math.round(r.PrivateMemorySize64 / 1e6), cpuSec: Math.round(r.CPU ?? 0) };
    }
    sample.totalWsMb = total;
    appendFileSync(join(STATS, 'resources.jsonl'), JSON.stringify(sample) + '\n');
    writeFileSync(join(STATS, 'resources.json'), JSON.stringify(sample, null, 2));
  });
}

async function stopAll(reason) {
  if (stopping) return;
  stopping = true;
  say('stopping', { reason });
  for (const c of children.values()) {
    if (!c.child) continue;
    try {
      // taskkill /T: the whole tree (node children, executor threads)
      execFile('taskkill', ['/PID', String(c.pid), '/T', '/F'], { windowsHide: true }, () => {});
    } catch {
      /* already gone */
    }
  }
  setTimeout(() => {
    writeStatus();
    rmSync(join(RUN, 'supervisor.pid'), { force: true });
    say('stopped');
    process.exit(0);
  }, 3000);
}

writeFileSync(join(RUN, 'supervisor.pid'), String(process.pid));
say('supervisor up', { pid: process.pid, run: RUN });
for (const s of specs()) start(s);
setInterval(writeStatus, 10_000);
setInterval(sampleResources, 60_000);

let diskWarnedAt = 0;
function checkDisk() {
  let free;
  try {
    const f = statfsSync(RUN);
    free = Number(f.bavail) * Number(f.bsize);
  } catch (e) {
    return;
  }
  const now = Date.now();
  writeFileSync(join(STATS, 'disk.json'), JSON.stringify({ ts: now, freeGb: +(free / GB).toFixed(2), lowDisk }));
  if (free < DISK_WARN && now - diskWarnedAt > 10 * 60_000) {
    diskWarnedAt = now;
    say('disk space low', { freeGb: +(free / GB).toFixed(2), warnBelowGb: DISK_WARN / GB, lowModeBelowGb: DISK_LOW / GB });
  }
  if (!lowDisk && free < DISK_LOW) {
    lowDisk = true;
    say('low-disk mode on: logs rotated and gzipped, bot / checker info lines dropped', { freeGb: +(free / GB).toFixed(2) });
    for (const c of children.values()) {
      try {
        c.log.rotate();
      } catch (e) {
        say('rotate failed', { name: c.spec.name, error: String(e) });
      }
    }
  } else if (lowDisk && free > DISK_LOW + GB) {
    lowDisk = false;
    say('low-disk mode off', { freeGb: +(free / GB).toFixed(2) });
  }
}
checkDisk();
setInterval(checkDisk, 60_000);
setTimeout(sampleResources, 15_000);
setInterval(() => (existsSync(STOP_FILE) ? stopAll('STOP file') : restartRequests()), 2000);
process.on('SIGINT', () => stopAll('SIGINT'));
process.on('SIGTERM', () => stopAll('SIGTERM'));
