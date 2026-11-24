// Writes the deployment record of a network. scripts/build-deploy.sh calls it from the repo root.
//
//   node scripts/lib/deploy.mjs <network> <out dir>
//
// No template depends on a network or a genesis (a stop is armed by a fill in the same
// transaction), so a deployment deploys the reference artifacts under contracts/artifacts
// unchanged. The record lists them (name, artifact, template hash, prefix and suffix lengths), so
// that a binary built for the network (cargo feature deploy-<network>) can check what it embeds. It
// writes <out dir>/deployment.json and <out dir>/DEPLOYMENT.md.
//
// When registry/tokens.json is the registry of this network (its `network` field), the record also
// pins it:
// - its path,
// - the sha256 of its bytes (line endings normalised to LF, as SHA256SUMS hashes them),
// - its strict template list (the token programs the build supports),
// - its token entries with their genesis-check fact.
// The registry is a snapshot of facts, with no listing state and no standing. A deploy-mainnet
// binary embeds the same file and checks the pin, so a changed registry makes the record stale.
import { createHash } from 'node:crypto';
import { execSync } from 'node:child_process';
import fs from 'node:fs';

const [network, out] = process.argv.slice(2);
const die = (m) => {
  console.error(`build-deploy: ${m}`);
  process.exit(1);
};
if (!/^[a-z0-9-]+$/.test(network || '')) die(`bad network ${network}`);
const readLf = (f) => fs.readFileSync(f, 'utf8').replace(/\r/g, '');
const toHex = (b) => Buffer.from(b).toString('hex');

// The KOB order kinds: the KOB1 kind codes 0x01..0x06 and 0x08..0x0b. Code 0x07, a retired receipt
// kind, is unused. They are the six plain, conditional and if-done kinds in both families, the
// three pair templates that serve both (KobPair, KobCondPair, KobIfdPair) and the KCC-20 curve
// (KobCurveAsk).
const KINDS = ['KobAsk', 'KobBid', 'KobCondAsk', 'KobCondBid', 'KobIfdBid', 'KobIfdAsk'];
// A test network also gets the delivery id_type 0 builds of the KRON kinds (`*KronD0`, for token
// programs whose holders are id_type 0: KForge on testnet-10). The mainnet record does not list
// them (kob_protocol::artifacts::deployed_order_kinds).
const names = [
  ...KINDS,
  'KobPair',
  'KobCondPair',
  'KobIfdPair',
  'KobCurveAsk',
  ...KINDS.map((k) => `${k}Kron`),
  ...(network === 'mainnet' ? [] : KINDS.map((k) => `${k}KronD0`)),
];
const templateOf = (name, file) => {
  if (!fs.existsSync(file)) die(`${file} is missing`);
  const c = JSON.parse(readLf(file)).contracts[name];
  if (!c) die(`${file} has no contract ${name}`);
  const { bytecode, template_hash, state_span } = c.compiled;
  return {
    name,
    artifact: file,
    hash: toHex(template_hash),
    prefix_len: state_span.offset,
    suffix_len: bytecode.length - state_span.offset - state_span.len,
  };
};
const templates = names.map((name) => templateOf(name, `contracts/artifacts/${name}.json`));
// The lock templates: every KobLockAuto template a build ever issued (docs/spec/template-retirement.md,
// "Lock templates"). A lock lasts up to 10 years, so the list only grows: the entries of the committed
// record are carried over, each still checked against its artifact (a re-pin keeps the old artifact
// under a new file name), and the current template is added when it is new.
const LOCKS = ['KobLockAuto'];
const recordFile = `contracts/deploy/${network}/deployment.json`;
const locks = (fs.existsSync(recordFile) ? JSON.parse(readLf(recordFile)).locks ?? [] : []).map((l) => {
  const t = templateOf(l.name, l.artifact);
  if (t.hash !== l.hash || t.prefix_len !== l.prefix_len || t.suffix_len !== l.suffix_len) {
    die(`lock template ${l.name} ${l.hash}: ${l.artifact} is now another template; an issued lock stays readable, so keep its artifact under a new name and point the record at it`);
  }
  return t;
});
for (const name of LOCKS) {
  const t = templateOf(name, `contracts/artifacts/${name}.json`);
  if (!locks.some((l) => l.hash === t.hash)) locks.push(t);
}
// Withdrawn lock templates (audit 2026-11-24 R6T-5): templates a committed record listed but no
// build ever issued (a re-pin committed on a branch and dropped before any deployment). The history
// check below takes them as accounted for. They are entered by hand in the committed record, each
// with why, and carried over like `locks`; an entry may never name a template that is in `locks`
// (the current one included). docs/spec/template-retirement.md, "Withdrawn lock templates".
const withdrawn = (fs.existsSync(recordFile) ? JSON.parse(readLf(recordFile)).withdrawn_locks ?? [] : []).map((w) => {
  if (typeof w?.name !== 'string' || !/^[0-9a-f]{64}$/.test(w.hash ?? '') || typeof w.why !== 'string' || !w.why.trim()) {
    die(`withdrawn_locks in ${recordFile}: each entry is { "name", "hash" (64 lower-case hex), "why" (the commits that listed it and the proof that no build issued it) }`);
  }
  if (locks.some((l) => l.hash === w.hash)) {
    die(`lock template ${w.name} ${w.hash} is both in locks and in withdrawn_locks: a template a build issues, or that this build issues, is never withdrawn`);
  }
  return { name: w.name, hash: w.hash, why: w.why };
});
// An entry rests on the operator's word alone: every build says each one aloud, so a review of the
// build log sees it (R7T-4).
for (const w of withdrawn) {
  console.error(`build-deploy: WITHDRAWN lock template ${w.name} ${w.hash} (${recordFile} withdrawn_locks, on the operator's word that no build issued it): ${w.why}`);
}
// A hand edit that deletes an entry would pass the checks above: every lock template any committed
// record of this network listed must still be listed (audit 2026-11-23 R2A-3). The check needs the
// whole history of the record: a shallow clone (actions/checkout's default, depth 1) holds only the
// record itself, and the check would compare it with itself (audit 2026-11-24 R3F-5). So a shallow
// clone or a git error fails; KOB_DEPLOY_NO_HISTORY=1 skips the check and says so (a tree without
// history, e.g. an archive, or a CI job whose commit job contracts of ci.yml checks with full history).
if (process.env.KOB_DEPLOY_NO_HISTORY === '1') {
  console.error(`build-deploy: KOB_DEPLOY_NO_HISTORY=1: the lock-history check of ${recordFile} is skipped`);
} else {
  const git = (c) => {
    try {
      return execSync(c, { encoding: 'utf8', maxBuffer: 64 << 20, stdio: ['ignore', 'pipe', 'pipe'] });
    } catch (e) {
      return die(`the lock-history check needs git and the full history (${c} failed: ${String(e.stderr || e.message).trim()}); KOB_DEPLOY_NO_HISTORY=1 skips it in a tree without history`);
    }
  };
  if (git('git rev-parse --is-shallow-repository').trim() !== 'false') {
    die(`the lock-history check needs the full history of ${recordFile}, and this clone is shallow: git fetch --unshallow (actions/checkout: fetch-depth: 0)`);
  }
  // No diff filter: without a diff format git log selects no commit by one (audit 2026-11-24
  // R4F-1). A commit that deleted the record has no copy of it to read, so it is skipped.
  // --full-history: by default git follows only the parent a merge is TREESAME to, so a merge that
  // took a branch's copy hides every commit behind its other parent (audit 2026-11-24 R5F-1). With it
  // every commit that changed the record on any path into HEAD is read, whatever the merge direction;
  // --first-parent would depend on it. A lock dropped on a branch before its merge then refuses too.
  const commits = git(`git log --full-history --format=%h -- ${recordFile}`).split('\n').filter(Boolean);
  if (!commits.length && git(`git ls-files -- ${recordFile}`).trim()) die(`git log lists no commit of the tracked ${recordFile}: the lock-history check would compare nothing`);
  for (const c of commits) {
    try {
      execSync(`git cat-file -e ${c}:${recordFile}`, { stdio: 'ignore' });
    } catch {
      continue;
    }
    const issued = JSON.parse(git(`git show ${c}:${recordFile}`)).locks ?? [];
    for (const l of issued) {
      if (!locks.some((x) => x.hash === l.hash) && !withdrawn.some((w) => w.hash === l.hash)) {
        die(`lock template ${l.name} ${l.hash} was issued (${recordFile} at ${c} lists it) and is missing from locks: an issued lock stays readable for up to 10 years, so its entry and its artifact stay. Only if no build was ever deployed with it (listed on a branch and dropped before any release) may it go to withdrawn_locks (docs/spec/template-retirement.md, "Withdrawn lock templates")`);
      }
    }
  }
}
// The token registry of this network, when the repository ships one (registry/tokens.json is the
// mainnet registry).
const REGISTRY = 'registry/tokens.json';
let registry = null;
if (fs.existsSync(REGISTRY)) {
  const text = readLf(REGISTRY);
  const doc = JSON.parse(text);
  if (doc.network === network) {
    registry = {
      path: REGISTRY,
      sha256: createHash('sha256').update(text, 'utf8').digest('hex'),
      schema_version: doc.schema_version,
      strict_templates: doc.templates.filter((t) => t.review_status === 'reviewed').map((t) => ({ id: t.id, template_hash: t.template_hash })),
      pending_templates: doc.templates.filter((t) => t.review_status !== 'reviewed').map((t) => t.id),
      tokens: doc.tokens.map((t) => ({ ticker: t.ticker, covenant_id: t.covenant_id, genesis_verified: t.genesis_verified ?? null })),
    };
  }
}
// The default indexer of a build for this network (the official index-only server, `null` until its
// address is decided). It is set by editing `indexer_url` in the committed record, which this
// script carries over, so `--check` reproduces it. The rule is the same as
// kob_protocol::artifacts::check_indexer_url: https on mainnet, a host, no credentials, query or
// fragment.
let indexerUrl = null;
const previous = `contracts/deploy/${network}/deployment.json`;
if (fs.existsSync(previous)) {
  const v = JSON.parse(readLf(previous)).indexer_url;
  if (typeof v === 'string') indexerUrl = v;
}
if (indexerUrl !== null) {
  const [scheme, rest = ""] = indexerUrl.split("://");
  const authority = rest.split("/")[0];
  if (scheme !== "https" && !(scheme === "http" && network !== "mainnet")) die(`indexer_url ${indexerUrl}: must be an https:// URL on mainnet (http:// or https:// elsewhere)`);
  if (!authority || authority.startsWith(":")) die("indexer_url has no host");
  if (authority.includes("@")) die("indexer_url must not carry credentials");
  if (["?", "#", " "].some((c) => indexerUrl.includes(c)) || indexerUrl !== indexerUrl.trim()) die("indexer_url must not carry a query, a fragment or whitespace");
}
const manifest = {
  network,
  protocol: 'v3 (no lots: base units, prices per whole base token; pair orders A/B, one template per pair kind for both families)',
  compiler: `silverc ${readLf('contracts/silverc.lock').match(/^version (\S+)$/m)[1]} (contracts/silverc.lock)`,
  note: 'every template is the reference artifact under contracts/artifacts; nothing is substituted per network',
  indexer_url: indexerUrl,
  templates,
  locks,
  ...(withdrawn.length ? { withdrawn_locks: withdrawn } : {}),
  ...(registry ? { registry } : {}),
};
fs.mkdirSync(out, { recursive: true });
fs.writeFileSync(`${out}/deployment.json`, JSON.stringify(manifest, null, 2) + '\n');

const rows = templates.map((t) => `| ${t.name} | \`${t.hash}\` | ${t.prefix_len} / ${t.suffix_len} |`);
fs.writeFileSync(
  `${out}/DEPLOYMENT.md`,
  `# KOB deployment: ${network}

Generated by \`scripts/build-deploy.sh ${network}\` (do not edit; \`--check\` reproduces it).

| | |
|---|---|
| Network | ${network} |
| Protocol | ${manifest.protocol} |
| Compiler | ${manifest.compiler} |
| Default indexer | ${indexerUrl ? '`' + indexerUrl + '`' : 'not set (clients name one with `--indexer` / `$KOB_INDEXER_URL`)'} |

A stop, stop-limit, trailing stop, OCO stop leg or stop entry is armed by the fill of a plain resting order in
the same transaction (touch trigger), so no template depends on a network and there is no genesis step: the
templates below are the reference artifacts of \`contracts/artifacts\`, final at build time, the same on
every network.

| Template | Template hash | prefix / suffix |
|---|---|---|
${rows.join('\n')}

Lock templates: every automatic creator lock template (\`KobLockAuto\`) a build ever issued. A lock lasts up to 10 years,
so this list only grows, and every build reads and releases each one (docs/spec/template-retirement.md, "Lock templates").

| Lock template | Template hash | prefix / suffix |
|---|---|---|
${locks.map((t) => `| ${t.name} | \`${t.hash}\` | ${t.prefix_len} / ${t.suffix_len} |`).join('\n')}${withdrawn.length ? `

Withdrawn lock templates: listed by an earlier record, never issued by a build (docs/spec/template-retirement.md).

| Withdrawn lock template | Template hash | Why |
|---|---|---|
${withdrawn.map((w) => `| ${w.name} | \`${w.hash}\` | ${w.why.replace(/\|/g, '\\|')} |`).join('\n')}` : ''}

The token programs are the reference artifacts too. Binaries embedding this record: see "Deployment
records" in contracts/README.md.
${registry ? registrySection(registry) : ''}`,
);

function registrySection(r) {
  const code = (s) => '`' + s + '`';
  const check = (g) => (g === true ? 'every genesis output checked' : g === false ? 'checked, not clean' : 'not checked');
  const tokens = r.tokens.map((t) => `| ${t.ticker} | ${code(t.covenant_id)} | ${check(t.genesis_verified)} |`);
  return `
## Token registry (pinned)

| | |
|---|---|
| File | ${code(r.path)} (schema ${r.schema_version}) |
| sha256 (LF) | ${code(r.sha256)} |
| Supported programs (strict templates) | ${r.strict_templates.map((t) => `${t.id} ${code(t.template_hash)}`).join(', ') || 'none'} |
| Pending review | ${r.pending_templates.join(', ') || 'none'} |

The deploy-${network} build embeds this file as its default registry and checks this hash. An operator may load another
registry (${code('--tokens')}): its hash is reported by ${code('GET /v1/tokens')}. The entries below are facts, not a
listing: every token of a supported program trades whether or not it has an entry.

| Token entry | Covenant id | Genesis check |
|---|---|---|
${tokens.join('\n')}
`;
}
for (const t of [...templates, ...locks]) console.log(`${t.name.padEnd(16)} ${t.hash}`);
