// The automatic creator lock in the web app (kob-protocol lock_auto.rs): a creation with the lock
// planned by default as automatic (its 1 KAS coin in the genesis, the declaration's `lock_kind`),
// the launch proof's verified lock with its coin, both confirmations taking the coin as the wallet's
// own locked KAS (and only when the declaration derives it), the release built with no signature,
// finalized and run in the script engine, the decoder's reading of it, the Portfolio's planner and
// the send path that skips the wallet when nothing is to be signed.
import { describe, expect, it, vi } from 'vitest';
import { loadKobNode } from './wasm.node';
import { pubkeyOf, signBuilt } from '../testing/local-signer';
import { AUTO_COIN, MAX_TAKE, buildRelease, creatorLock, declaredLock, verifiedLockOf, type VerifiedLock } from './creator-lock';
import { decodeDeclaration, verifyLaunchProof } from './kcc23';
import { emptyIssueForm, metadataBytes, planIssue, type IssueForm, type IssuePlan } from './issue';
import { defaultSaleForm } from './curve';
import { decodeSigning } from './decode';
import { isTokenProgram } from './programs.generated';
import { outpointKey, type NodeInputFact } from './node-verify';
import type { BuiltTx, Kcc20State, KeyUtxo, TokenUtxo } from './types';
import { buildIssuanceModel } from '../ui/confirm/issuance-model';
import { creationSummary, summarize } from '../signer/summary';
import { genesisClaimOf } from '../wallet/create';
import { signAndSubmit } from '../wallet/sign';
import type { WalletAdapter } from '../wallet/types';
import type { Services } from '../app/services';
import type { LockedHolding } from '../ui/orders/balances-data';
import { planRelease } from '../ui/orders/unlock';
import { describeActionError } from '../ui/orders/actions';
import { reviewSummary } from '../ui/issue/issue-model';
import { t } from '../i18n';

const kob = loadKobNode();
const KAS = 100_000_000n;
const SK = '35'.repeat(32);
const maker = pubkeyOf(SK);
const other = pubkeyOf('46'.repeat(32));
const ZERO = '00'.repeat(32);
const DAY = 86_400_000;
const until = String(Date.now() + 90 * DAY);

const utxo = (index: number, sompi: bigint, pubkey = maker): KeyUtxo => ({ transactionId: '07'.repeat(32), index, amount: sompi.toString(), blockDaaScore: '5000', pubkey });
const form = (over: Partial<IssueForm> = {}): IssueForm => ({ ...emptyIssueForm(), name: 'Auto Lock', ticker: 'ALCK', decimals: 8, supply: '1000', lockUntil: until, ...over });
const plan = (over: Partial<IssueForm> = {}): IssuePlan => planIssue(kob, form(over), { funding: [utxo(1, 500n * KAS)], maker, network: 'testnet-10' });
const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x)) as T;
const factsOf = (built: BuiltTx): Map<string, NodeInputFact> =>
  new Map(built.tx.inputs.map((i) => [outpointKey(i.transactionId, i.index), { amount: BigInt(i.utxo.amount), scriptPublicKey: i.utxo.scriptPublicKey, covenantId: i.utxo.covenantId ?? null }]));

describe('the automatic lock', () => {
  it('is its own script and address, from the same terms as the manual one', () => {
    const manual = creatorLock(kob, { until: '1800000000000', key: maker });
    const auto = creatorLock(kob, { until: '1800000000000', key: maker, kind: 'auto', program: 'KCC20PublicMint' });
    expect(manual.kind).toBe('manual');
    expect(auto).toMatchObject({ kind: 'auto', until: '1800000000000', key: maker, terms: manual.script });
    expect(auto.script).not.toBe(manual.script);
    expect(auto.authority).not.toBe(manual.authority);
    expect(auto.scriptPublicKey).toBe(`0000aa20${auto.authority}87`);
    // another program, another script
    expect(creatorLock(kob, { until: '1800000000000', key: maker, kind: 'auto', program: 'KCC20Ref' }).authority).not.toBe(auto.authority);
    // from a declaration: the terms and the kind, with the program the outputs are checked under
    expect(declaredLock(kob, { lock: manual.script, lockKind: 'auto' }, 'KCC20PublicMint')).toEqual(auto);
    expect(declaredLock(kob, { lock: manual.script, lockKind: null }, 'KCC20PublicMint')).toEqual(manual);
    expect(declaredLock(kob, { lock: manual.script, lockKind: 'auto' }, null)).toBeNull();
    expect(declaredLock(kob, { lock: null, lockKind: 'auto' }, 'KCC20PublicMint')).toBeNull();
  });
});

describe('a creation with an automatic lock (the default)', () => {
  const p = plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '40', price: '1' } });

  it('locks the kept share under KobLockAuto and pays the 1 KAS coin to its address', () => {
    expect(p.lock).toMatchObject({ kind: 'auto', holder: 1, until, key: maker });
    const lock = creatorLock(kob, { until, key: maker, kind: 'auto', program: p.token.program });
    expect(p.lock!.authority).toBe(lock.authority);
    expect(p.token.outputs[1]).toMatchObject({ ownerScheme: 3, owner: lock.authority });
    const at = p.lock!.coinOutput!;
    expect(at).toBeGreaterThan(1);
    expect(p.built.tx.outputs[at]).toMatchObject({ value: AUTO_COIN.toString(), scriptPublicKey: lock.scriptPublicKey });
    expect(p.built.tx.outputs[at]!.covenant ?? null).toBeNull();
    // the declaration names the terms and the kind
    expect(p.declaration.json).toContain(`"lock":"${lock.terms}","lock_kind":"auto"`);
    expect(metadataBytes(form())).toBe(1 + new TextEncoder().encode(p.declaration.json).length);
    const d = decodeDeclaration(kob, p.prep.tx.payload).token;
    expect([d.lock, d.lockKind]).toEqual([lock.terms, 'auto']);
    for (const b of [p.prep, p.built]) kob.validate(kob.finalize(b, signBuilt(b, [SK]), { tightenBudgets: true }));
  });

  it('the review says it opens automatically and shows the coin', () => {
    const r = reviewSummary(p, maker);
    expect(r.lock).toMatchObject({ kind: 'auto', coin: AUTO_COIN, amount: 400n * 10n ** 8n });
  });

  it('the launch proof binds the lock with its kind and coin', () => {
    const v = verifyLaunchProof(kob, p.docs.launchProof);
    expect(v.lock).toMatchObject({ kind: 'auto', until, key: maker, authority: p.lock!.authority, coin: { index: p.lock!.coinOutput, amount: AUTO_COIN.toString() } });
    expect(verifiedLockOf(v.lock, isTokenProgram)).toEqual(v.lock);
    // a coin at a token output, or none, is no automatic lock here
    expect(verifiedLockOf({ ...v.lock!, coin: null }, isTokenProgram)).toBeNull();
    expect(verifiedLockOf({ ...v.lock!, coin: { index: v.lock!.outputs[0]!.index, amount: AUTO_COIN.toString() } }, isTokenProgram)).toBeNull();
    // an older binding's lock (no kind) is the manual one, its script the template
    const { kind: _k, coin: _c, ...old } = v.lock!;
    expect(verifiedLockOf(old, isTokenProgram)).toBeNull();
  });

  it('the app confirmation: the coin is the wallet\'s KAS, locked, and nothing blocks', () => {
    const m = buildIssuanceModel({ kob, built: p.built, prep: p.prep, oneApproval: true, token: p.token, maker, tr: (k, q) => t(k, q) });
    expect(m.blocking).toEqual([]);
    expect(m.brief!.find((r) => r.id === 'lock')?.detail).toMatch(/opens automatically$/);
    const locked = m.sections.find((s) => s.id === 'locked')!.rows;
    expect(locked.find((r) => r.id === 'locked-lock-coin')?.value).toBe('1 KAS');
    // seen by another wallet: the coin pays somewhere else
    const theirs = buildIssuanceModel({ kob, built: p.built, prep: p.prep, oneApproval: true, token: p.token, maker: other, tr: (k, q) => t(k, q) });
    expect(theirs.blocking.length).toBeGreaterThan(0);
    // a declaration without lock_kind: the coin is to an unknown script
    const plain = clone(p.prep);
    const json = p.declaration.json.replace(',"lock_kind":"auto"', '');
    plain.tx.payload = `00${Buffer.from(json, 'utf8').toString('hex')}`;
    const manual = buildIssuanceModel({ kob, built: p.built, prep: plain, oneApproval: true, token: p.token, maker, tr: (k, q) => t(k, q) });
    expect(manual.blocking.map((f) => f.code)).toContain('outputOther');
  });

  it('the signer frame derives the coin from the declaration and the claimed program', () => {
    const s = creationSummary(kob, p.prep, p.built, maker, [], genesisClaimOf(p.token));
    expect(s.problems).toBe(0);
    const line = (re: RegExp) => s.lines.find((l) => re.test(l.label));
    expect(line(/^Locked for you until .* opens automatically$/)?.value).toBe('400 ALCK');
    expect(line(/^Lock coin/)?.value).toBe('1 KAS');
    expect(line(/^Locked in orders/)).toBeUndefined();
    // the bare decoder knows nothing of the coin: an unknown script
    expect(summarize(kob, p.built, maker, []).decoded.blocking.map((b) => b.code)).toContain('output-unknown');
    // two coins, or another amount, block
    const twice = clone(p.built);
    twice.tx.outputs.push(clone(twice.tx.outputs[p.lock!.coinOutput!]!));
    const d2 = decodeSigning({ kob, built: twice, maker, autoLocks: [{ until, key: maker, program: p.token.program }] });
    expect(d2.blocking.map((b) => b.code)).toContain('lock-coin-stranded');
  });
});

describe('the release', () => {
  const p = plan();
  const lo = p.token.outputs[0]!;
  const state: Kcc20State = { amount: lo.amount, owner: lo.owner, owner_scheme: 3, borrow_scheme: 0, borrow_guard: ZERO, extension_commitment: p.token.extensionCommitment };
  const tokens: TokenUtxo = { transactionId: p.built.tx.id, index: lo.index, amount: p.built.tx.outputs[lo.index]!.value, covenantId: p.token.covenantId, state };
  const coin = { transactionId: p.built.tx.id, index: p.lock!.coinOutput!, amount: AUTO_COIN.toString() };
  const token = { covenantId: p.token.covenantId, program: p.token.program };
  const release = buildRelease(kob, { token, lock: { until, key: maker }, tokens, coin });

  it('is built with nobody to sign, finalized with no signature and passes the script engine', () => {
    expect(release.sign).toEqual([]);
    expect(release.plans[0]).toEqual({ kind: 'creatorLockAuto', until, key: maker, program: p.token.program });
    expect(release.plans[1]).toMatchObject({ kind: 'tokenLeader', witness: { kind: 'p2sh', value: 0 } });
    expect(release.tx.lockTime).toBe(until);
    expect(AUTO_COIN - BigInt(release.tx.outputs[1]!.value)).toBeLessThanOrEqual(MAX_TAKE);
    kob.validate(kob.finalize(release, [], { tightenBudgets: true }));
  });

  it('decodes as the wallet\'s tokens unlocked and the coin\'s KAS released to it', () => {
    const s = decodeSigning({ kob, built: release, maker, nodeInputs: factsOf(release) });
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('unlock');
    expect(s.inputs[0]).toMatchObject({ type: 'kas', ownedBy: 'maker', willSign: false, signer: null, autoLockCoin: { until: BigInt(until) } });
    expect(s.net.tokens[0]).toMatchObject({ unlocked: 1000n * 10n ** 8n, toMaker: 1000n * 10n ** 8n, toOthers: 0n });
    expect(s.net.kas.released).toBe(AUTO_COIN);
    expect(s.net.kas.walletDelta).toBe(AUTO_COIN - s.net.kas.fee);
    expect(s.info.map((i) => i.code)).toEqual(expect.arrayContaining(['unlock-tokens', 'release-kas']));
  });

  it('blocks a coin of another lock, an early lock time and a coin of another genesis', () => {
    const swapped = clone(release);
    swapped.tx.inputs[0]!.utxo.scriptPublicKey = creatorLock(kob, { until, key: maker }).scriptPublicKey;
    expect(decodeSigning({ kob, built: swapped, maker, nodeInputs: factsOf(swapped) }).blocking.map((b) => b.code)).toContain('input-script-mismatch');
    const early = clone(release);
    early.tx.lockTime = String(BigInt(until) - 1n);
    expect(decodeSigning({ kob, built: early, maker, nodeInputs: factsOf(early) }).blocking.map((b) => b.code)).toContain('release-invalid');
    expect(() => buildRelease(kob, { token, lock: { until, key: maker }, tokens, coin: { ...coin, transactionId: '09'.repeat(32) } })).toThrow(/same genesis/);
    // the tokens rerouted to another key
    const away = clone(release);
    away.tx.outputs[1]!.scriptPublicKey = `000020${other}ac`;
    expect(decodeSigning({ kob, built: away, maker, nodeInputs: factsOf(away) }).blocking.map((b) => b.code)).toContain('release-invalid');
  });

  it('the Portfolio plans it from the verified lock, after the node clock passes the date', async () => {
    const v = verifyLaunchProof(kob, p.docs.launchProof).lock as VerifiedLock;
    const holding: LockedHolding = {
      token: p.token.covenantId, program: v.program, until: BigInt(v.until), key: v.key, authority: v.authority, utxos: [tokens], amount: BigInt(v.amount), kind: 'auto',
      coin: { transactionId: p.built.tx.id, index: v.coin!.index, amount: v.coin!.amount },
    };
    const services = (median: bigint | null) =>
      ({ kob, config: { network: 'testnet-10' }, node: { getPastMedianTime: async () => median, getFeeEstimate: async () => null }, fees: null }) as unknown as Services;
    const early = await planRelease(services(BigInt(until) - 61_000n), maker, holding, 'ALCK').catch((e: unknown) => e);
    expect(describeActionError(early)).toMatch(/^Not open yet on the network: the node clock is 2 min/);
    const run = await planRelease(services(BigInt(until) + 1n), maker, holding, 'ALCK');
    expect(run.steps).toHaveLength(1);
    expect(run.steps[0]!.plan.built!.sign).toEqual([]);
    expect(run.steps[0]!.plan.built!.tx.inputs.map((i) => `${i.transactionId}:${i.index}`)).toEqual([`${coin.transactionId}:${coin.index}`, `${tokens.transactionId}:${tokens.index}`]);
    // a manual lock has no release
    expect(describeActionError(await planRelease(services(BigInt(until) + 1n), maker, { ...holding, kind: 'manual', coin: null }, 'ALCK').catch((e: unknown) => e))).toBe('This creator lock does not open by itself.');
  });

  it('is sent without asking the wallet, still finalized, validated and checked by txid', async () => {
    const signTx = vi.fn();
    const adapter = { id: 'kasware', signTx, refresh: vi.fn() } as unknown as WalletAdapter;
    const node = { submitTransaction: vi.fn(async (tx: { id: string }) => tx.id) } as never;
    const res = await signAndSubmit({ kob, node, adapter, built: release, network: 'testnet-10' });
    expect(signTx).not.toHaveBeenCalled();
    expect(res.txid).toBe(release.tx.id);
  });
});
