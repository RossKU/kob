// Creator lock in the web app: the template parser against kob-wasm, a creation with a lock planned and verified (its launch proof binds the
// lock with its amount, outputs and supply), the confirmations of that creation (app and signer frame: "locked for you", never "another key"),
// and the pre-sign decoder on an unlock and a lock coin: ok for the wallet's own lock, blocking for another key's or a wrong one.
import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { pubkeyOf, signBuilt } from '../testing/local-signer';
import { LOCK_COIN, buildLockCoin, buildUnlock, creatorLock, lockOfScript, lockSharePercent, parseLockScript, verifiedLockOf } from './creator-lock';
import { decodeDeclaration, verifyLaunchProof } from './kcc23';
import { emptyIssueForm, metadataBytes, IssueFormError, planIssue, type IssueForm, type IssuePlan } from './issue';
import { defaultSaleForm } from './curve';
import { decodeSigning } from './decode';
import { isTokenProgram } from './programs.generated';
import { outpointKey, type NodeInputFact } from './node-verify';
import type { BuiltTx, Kcc20State, KeyUtxo, TokenUtxo } from './types';
import { buildIssuanceModel } from '../ui/confirm/issuance-model';
import { creationSummary, summarize } from '../signer/summary';
import { genesisClaimOf } from '../wallet/create';
import { t } from '../i18n';

const kob = loadKobNode();
const KAS = 100_000_000n;
const SK = '33'.repeat(32);
const maker = pubkeyOf(SK);
const OTHER_SK = '44'.repeat(32);
const other = pubkeyOf(OTHER_SK);
const ZERO = '00'.repeat(32);
const DAY = 86_400_000;
const until = String(Date.now() + 90 * DAY);

const utxo = (index: number, sompi: bigint, pubkey = maker): KeyUtxo => ({ transactionId: '07'.repeat(32), index, amount: sompi.toString(), blockDaaScore: '5000', pubkey });
const form = (over: Partial<IssueForm> = {}): IssueForm => ({ ...emptyIssueForm(), name: 'Lock Token', ticker: 'LOCK', decimals: 8, supply: '1000', lockUntil: until, ...over });
const plan = (over: Partial<IssueForm> = {}): IssuePlan => planIssue(kob, form(over), { funding: [utxo(1, 500n * KAS)], maker, network: 'testnet-10' });
const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x)) as T;

/** every input's node facts as the transaction claims them (the decoder compares them) */
function factsOf(built: BuiltTx): Map<string, NodeInputFact> {
  return new Map(built.tx.inputs.map((i) => [outpointKey(i.transactionId, i.index), { amount: BigInt(i.utxo.amount), scriptPublicKey: i.utxo.scriptPublicKey, covenantId: i.utxo.covenantId ?? null }]));
}

/** finalize with local keys and run the script engine */
function validates(built: BuiltTx, keys: string[] = [SK]): void {
  kob.validate(kob.finalize(built, signBuilt(built, keys), { tightenBudgets: true }));
}

describe('the template', () => {
  const l = creatorLock(kob, { until: '1800000000000', key: maker });
  const flip = (at: number, b: number) => l.script.slice(0, 2 * at) + b.toString(16).padStart(2, '0') + l.script.slice(2 * at + 2);
  const le = (v: bigint) => Array.from({ length: 8 }, (_, i) => ((v >> BigInt(8 * i)) & 0xffn).toString(16).padStart(2, '0')).join('');

  it('parses back to its terms, as kob-wasm does', () => {
    expect(l.script).toHaveLength(88);
    expect(parseLockScript(l.script)).toEqual({ until: 1_800_000_000_000n, key: maker });
    expect(creatorLock(kob, { script: l.script })).toEqual(l);
    expect(l.scriptPublicKey).toBe(`0000aa20${l.authority}87`);
  });

  it('refuses every look-alike, in agreement with kob-wasm', () => {
    const lookalikes = [
      flip(43, 0xad), // CHECKSIGVERIFY
      flip(9, 0xb1), // CHECKSEQUENCEVERIFY
      flip(0, 0x07),
      flip(10, 0x21),
      `08${le(1_000_000n)}${l.script.slice(18)}`, // a DAA-score lock time (below 5 x 10^11)
      `08${le((1n << 53n))}${l.script.slice(18)}`, // past 2^53 - 1
      l.script.slice(0, 86), // one byte short
      `${l.script}75`, // one byte more
      `${l.script.slice(0, 22)}${'ff'.repeat(32)}ac`, // a key off the curve
    ];
    for (const s of lookalikes) {
      expect(parseLockScript(s), s).toBeNull();
      expect(() => creatorLock(kob, { script: s }), s).toThrow();
      expect(lockOfScript(kob, s)).toBeNull();
    }
  });

  it('declares no lock for a look-alike script, and names the issue', () => {
    const json = `{"name":"Fake","symbol":"FAKE","decimals":8,"lock":"${flip(43, 0xad)}"}`;
    const d = decodeDeclaration(kob, `00${Buffer.from(json, 'utf8').toString('hex')}`);
    expect(d.token.lock).toBeNull();
    expect(d.token.issues.join(' ')).toMatch(/lock/);
  });

  it('says the share of the supply', () => {
    expect(lockSharePercent(400n, 1000n)).toBe('40');
    expect(lockSharePercent(125n, 1000n)).toBe('12.5');
    expect(lockSharePercent(1n, 3n)).toBe('33.33');
    expect(lockSharePercent(1n, null)).toBeNull();
  });
});

describe('a creation with a lock', () => {
  it('keep mode: the whole share is locked, the proof binds it with its amount, outputs and supply', () => {
    const p = plan();
    expect(p.lock?.holder).toBe(0);
    expect(p.token.outputs).toHaveLength(1);
    const o = p.token.outputs[0]!;
    expect(o.ownerScheme).toBe(3);
    expect(o.owner).toBe(p.lock!.authority);
    expect(p.declaration.json).toContain(`"lock":"${p.lock!.script}"`);
    expect(metadataBytes(form())).toBe(1 + new TextEncoder().encode(p.declaration.json).length);
    validates(p.prep);
    validates(p.built);
    const v = verifyLaunchProof(kob, p.docs.launchProof);
    expect(v.token.lock).toBe(p.lock!.script);
    expect(v.lock).toMatchObject({ until, key: maker, authority: p.lock!.authority, amount: (1000n * 10n ** 8n).toString(), outputs: [{ index: 0, amount: o.amount }], supply: p.token.supply, program: 'KCC20PublicMint' });
    expect(verifiedLockOf(v.lock, isTokenProgram)).toEqual(v.lock);
  });

  it('ask mode: only the kept share is locked; the part for sale stays the wallet\'s', () => {
    const p = plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '40', price: '1' } });
    expect(p.sale?.mode).toBe('ask');
    expect(p.token.outputs.map((o) => [o.ownerScheme, o.amount])).toEqual([[0, (600n * 10n ** 8n).toString()], [3, (400n * 10n ** 8n).toString()]]);
    expect(p.lock?.holder).toBe(1);
    const v = verifyLaunchProof(kob, p.docs.launchProof);
    expect(v.lock?.amount).toBe((400n * 10n ** 8n).toString());
    expect(lockSharePercent(v.lock!.amount, v.lock!.supply)).toBe('40');
  });

  it('curve mode: the kept share after the curve custody is locked', () => {
    const p = plan({ supply: '1000000', sale: { ...defaultSaleForm(), mode: 'curve', keepPct: '10' } });
    expect(p.curve).toBeDefined();
    expect(p.token.outputs[p.lock!.holder]!.ownerScheme).toBe(3);
    expect(p.token.outputs[p.curve!.custody]!.ownerScheme).toBe(4);
    validates(p.built);
    expect(verifyLaunchProof(kob, p.docs.launchProof).lock?.amount).toBe((100_000n * 10n ** 8n).toString());
  });

  it('refuses a lock with nothing kept, and a date in the past', () => {
    const none = () => plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '0', price: '1' } });
    expect(none).toThrow(IssueFormError);
    try {
      none();
    } catch (e) {
      expect((e as IssueFormError).issues.map((i) => i.code)).toContain('lock.no_keep');
    }
    expect(() => plan({ lockUntil: String(Date.now() - DAY) })).toThrow(/future/);
    expect(() => plan({ lockUntil: '1000' })).toThrow(/date/);
  });

  it('the app confirmation says locked for you, never another key, and blocks nothing', () => {
    const p = plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '40', price: '1' } });
    const m = buildIssuanceModel({ kob, built: p.built, prep: p.prep, oneApproval: true, token: p.token, maker, tr: (k, q) => t(k, q) });
    expect(m.blocking).toEqual([]);
    const holder = m.sections.find((s) => s.id === 'issue')!.rows.find((r) => r.id === 'holder-1')!;
    expect(holder.detail).toMatch(/^locked for you until \d{4}-\d{2}-\d{2}$/);
    expect(holder.tone).toBe('good');
    expect(m.info.map((i) => i.code)).toContain('lockedForYou');
    expect(m.info.map((i) => i.code)).not.toContain('holdersOther');
    expect(m.brief!.find((r) => r.id === 'lock')?.value).toBe('400 LOCK');
    // seen by another wallet: the lock is not its own, the output is another key's
    const theirs = buildIssuanceModel({ kob, built: p.built, prep: p.prep, oneApproval: true, token: p.token, maker: other, tr: (k, q) => t(k, q) });
    expect(theirs.info.map((i) => i.code)).not.toContain('lockedForYou');
  });

  it('the signer frame says locked for you and finds no problem', () => {
    const p = plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '40', price: '1' } });
    const s = creationSummary(kob, p.prep, p.built, maker, [], genesisClaimOf(p.token));
    expect(s.problems).toBe(0);
    const line = (re: RegExp) => s.lines.find((l) => re.test(l.label));
    expect(line(/^Locked for you until /)?.value).toBe('400 LOCK');
    expect(line(/^To other keys/)).toBeUndefined();
    expect(line(/^To you/)?.value).toBe('+600 LOCK');
  });
});

describe('the unlock', () => {
  const p = plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '40', price: '1' } });
  const lo = p.token.outputs[1]!;
  const state: Kcc20State = { amount: lo.amount, owner: lo.owner, owner_scheme: 3, borrow_scheme: 0, borrow_guard: ZERO, extension_commitment: p.token.extensionCommitment };
  const locked: TokenUtxo = { transactionId: p.built.tx.id, index: lo.index, amount: p.built.tx.outputs[lo.index]!.value, covenantId: p.token.covenantId, state };
  const lock = { until, key: maker };
  const coin = { transactionId: '09'.repeat(32), index: 0, amount: LOCK_COIN.toString() };
  const unlock = buildUnlock(kob, { token: { covenantId: p.token.covenantId, program: p.token.program }, lock, tokens: [locked], coin, ownKeys: [maker] });

  it('is built as the protocol says: the coin first, the p2sh witness, lock time = until', () => {
    expect(unlock.plans[0]).toEqual({ kind: 'creatorLock', until, key: maker });
    expect(unlock.plans[1]).toMatchObject({ kind: 'tokenLeader', witness: { kind: 'p2sh', value: 0 } });
    expect(unlock.tx.lockTime).toBe(until);
    expect(unlock.sign[0]!.redeemScript).toBe(p.lock!.script);
    validates(unlock);
  });

  it('decodes for the wallet as its own tokens unlocked: +N, the coin back as change, nothing blocking', () => {
    const s = decodeSigning({ kob, built: unlock, maker, nodeInputs: factsOf(unlock) });
    expect(s.blocking).toEqual([]);
    expect(s.ok).toBe(true);
    expect(s.kind).toBe('unlock');
    expect(s.inputs[0]).toMatchObject({ type: 'kas', ownedBy: 'maker', willSign: true, signer: maker });
    expect(s.inputs[1]).toMatchObject({ type: 'token', ownedBy: 'maker' });
    const tok = s.net.tokens[0]!;
    expect([tok.unlocked, tok.fromWallet, tok.toMaker, tok.walletDelta, tok.toOthers]).toEqual([400n * 10n ** 8n, 0n, 400n * 10n ** 8n, 400n * 10n ** 8n, 0n]);
    expect(s.net.kas.walletDelta).toBe(-s.net.kas.fee);
    expect(s.info.map((i) => i.code)).toContain('unlock-tokens');
    // the frame's summary
    const sum = summarize(kob, unlock, maker, []);
    expect(sum.problems).toBe(0);
    expect(sum.title).toBe('Unlock your creator lock');
  });

  it('blocks for another key: the lock is not the wallet\'s', () => {
    const s = decodeSigning({ kob, built: unlock, maker: other, nodeInputs: factsOf(unlock) });
    expect(s.ok).toBe(false);
    expect(s.blocking.map((b) => b.code)).toEqual(expect.arrayContaining(['unlock-invalid', 'sign-foreign-key']));
    expect(s.inputs[1]!.ownedBy).toBe('other');
  });

  it('blocks a lock of another key spent by this wallet, a coin that is not the lock\'s, and the tokens to another key', () => {
    const theirs = buildUnlock(kob, { token: { covenantId: p.token.covenantId, program: p.token.program }, lock: { until, key: other }, tokens: [{ ...locked, state: { ...state, owner: creatorLock(kob, { until, key: other }).authority } }], coin });
    expect(decodeSigning({ kob, built: theirs, maker: other, nodeInputs: factsOf(theirs) }).ok).toBe(true);
    expect(decodeSigning({ kob, built: theirs, maker, nodeInputs: factsOf(theirs) }).blocking.map((b) => b.code)).toContain('unlock-invalid');
    // the coin's script swapped for another lock's
    const swapped = clone(unlock);
    swapped.tx.inputs[0]!.utxo.scriptPublicKey = creatorLock(kob, { until: '1800000000000', key: maker }).scriptPublicKey;
    expect(decodeSigning({ kob, built: swapped, maker, nodeInputs: factsOf(swapped) }).blocking.map((b) => b.code)).toContain('input-script-mismatch');
    // the tokens paid to another key
    const away = buildUnlock(kob, { token: { covenantId: p.token.covenantId, program: p.token.program }, lock, tokens: [locked], coin, to: other });
    expect(decodeSigning({ kob, built: away, maker, nodeInputs: factsOf(away) }).blocking.map((b) => b.code)).toContain('transfer-out');
    // a lock time before the lock's
    const early = clone(unlock);
    early.tx.lockTime = String(BigInt(until) - 1n);
    expect(decodeSigning({ kob, built: early, maker, nodeInputs: factsOf(early) }).blocking.map((b) => b.code)).toContain('unlock-invalid');
  });

  it('a lock coin is the wallet\'s own only for its own open lock', () => {
    const past = String(Date.now() - DAY);
    const tx = buildLockCoin(kob, { lock: { until: past, key: maker }, funding: [utxo(2, 5n * KAS)], ownKeys: [maker] });
    expect(tx.tx.outputs[0]!.value).toBe(LOCK_COIN.toString());
    validates(tx);
    const dec = (locks?: { until: string; key: string }[], key = maker) => decodeSigning({ kob, built: tx, maker: key, nodeInputs: factsOf(tx), ...(locks ? { locks } : {}) });
    // unknown script without the hint
    expect(dec().blocking.map((b) => b.code)).toContain('output-unknown');
    const ok = dec([{ until: past, key: maker }]);
    expect(ok.blocking).toEqual([]);
    expect(ok.kind).toBe('lock-coin');
    expect(ok.info.map((i) => i.code)).toContain('lock-coin');
    expect(ok.net.kas.walletDelta).toBe(-ok.net.kas.fee);
    // a lock that is not open yet locks the KAS: not the wallet's to spend now
    const future = buildLockCoin(kob, { lock: { until, key: maker }, funding: [utxo(2, 5n * KAS)] });
    expect(decodeSigning({ kob, built: future, maker, nodeInputs: factsOf(future), locks: [{ until, key: maker }] }).blocking.map((b) => b.code)).toContain('output-unknown');
    // another key's lock hinted: not taken
    const theirs = buildLockCoin(kob, { lock: { until: past, key: other }, funding: [utxo(2, 5n * KAS)] });
    expect(decodeSigning({ kob, built: theirs, maker, nodeInputs: factsOf(theirs), locks: [{ until: past, key: other }] }).blocking.map((b) => b.code)).toContain('output-unknown');
    // the frame's summary with its hint
    expect(summarize(kob, tx, maker, [], [{ until: past, key: maker }]).problems).toBe(0);
    expect(summarize(kob, tx, maker, []).problems).toBeGreaterThan(0);
  });
});

describe('the form and the Portfolio', () => {
  it('a number of days is fixed at the choice; a date is the start of that day; none is no lock', async () => {
    const { chooseLock, emptyUiState, formToIssueForm, lockUntilOf, reviewSummary } = await import('../ui/issue/issue-model');
    const now = Date.UTC(2026, 10, 18, 10, 0, 30);
    const l = chooseLock('90', emptyUiState().lock, now);
    expect(l.until).toBe(String(Math.ceil((now + 90 * DAY) / 60_000) * 60_000));
    expect(lockUntilOf(l)).toBe(l.until);
    expect(lockUntilOf({ choice: 'date', date: '2027-03-01', until: '' })).toBe(String(new Date(2027, 2, 1).getTime()));
    expect(lockUntilOf({ choice: 'date', date: '', until: '' })).toBe('0');
    expect(lockUntilOf(chooseLock('none', l))).toBe('');
    const ui = { ...emptyUiState(), name: 'Lock Token', ticker: 'LOCK', supply: '1000', lock: l };
    expect(formToIssueForm(ui, maker).lockUntil).toBe(l.until);
    expect(formToIssueForm({ ...ui, lock: chooseLock('none', l) }, maker).lockUntil).toBeUndefined();
    const p = plan({ sale: { ...defaultSaleForm(), mode: 'ask', keepPct: '40', price: '1' } });
    const r = reviewSummary(p, maker);
    expect(r.lock).toEqual({ amount: 400n * 10n ** 8n, human: '400', until: BigInt(until) });
    expect(r.holders.map((h) => [h.isWallet, h.lockedUntil])).toEqual([[true, undefined], [false, BigInt(until)]]);
  });

  it('a lock the indexer names is found without a visit to the token page, after its declaration verifies', async () => {
    const { loadLockedHoldings } = await import('../ui/orders/balances-data');
    const p = plan();
    const network = 'testnet-10';
    const asked: string[] = [];
    const indexer = {
      creatorLocks: async (key: string) => (asked.push(key), [{ covenant_id: p.token.covenantId }, { covenant_id: 'cd'.repeat(32) }]),
      declaration: async (id: string) => (id === p.token.covenantId ? p.docs.launchProof : null),
    };
    const seen: string[] = [];
    const tracker = {
      lockedTokenUtxos: async (l: { covenantId: string }) => {
        seen.push(l.covenantId);
        const o = p.token.outputs[0]!;
        return [{ transactionId: '09'.repeat(32), index: 0, amount: '1000000000', blockDaaScore: '1', covenantId: p.token.covenantId, state: { amount: o.amount } }];
      },
    };
    const store = new Map<string, string>();
    const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v), removeItem: (k: string) => void store.delete(k) };
    const g = globalThis as { localStorage?: unknown };
    const before = g.localStorage;
    g.localStorage = storage;
    try {
      const held = await loadLockedHoldings({ kob, tracker, config: { network }, indexer } as never, maker);
      expect(asked).toEqual([maker]);
      expect(seen).toEqual([p.token.covenantId]);
      expect(held.map((h) => [h.token, h.until])).toEqual([[p.token.covenantId, BigInt(until)]]);
    } finally {
      g.localStorage = before;
    }
  });

  it('a token held only by its lock still has its row, the locked amount apart from Available', async () => {
    const { combineBalances } = await import('../ui/orders/balances-model');
    const token = 'ab'.repeat(32);
    const c = combineBalances({ maker, freeKas: 0n, freeTokens: [], liveOrders: [], strays: [], locked: [{ token, amount: 400n, until: 1_900_000_000_000n }] });
    expect(c.tokens).toHaveLength(1);
    expect(c.tokens[0]).toMatchObject({ token, free: 0n, locked: { amount: 400n, until: 1_900_000_000_000n } });
  });
});

describe('the signer protocol', () => {
  it('takes lock hints on a plain request only, of the right shape', async () => {
    const { parseAppMessage, SIGNER_PROTOCOL } = await import('../signer/protocol');
    const p = plan();
    const coin = buildLockCoin(kob, { lock: { until: '1800000000000', key: maker }, funding: [utxo(2, 5n * KAS)] });
    const msg = (extra: Record<string, unknown>) => parseAppMessage(clone({ p: SIGNER_PROTOCOL, type: 'sign', id: 'r1', built: coin, tokens: [], ...extra }));
    expect(msg({ locks: [{ until: '1800000000000', key: maker }] })).toMatchObject({ locks: [{ until: '1800000000000', key: maker }] });
    expect(msg({ locks: [{ until: 1800000000000, key: maker }] })).toBeNull();
    expect(msg({ locks: [{ until: '1800000000000', key: 'zz' }] })).toBeNull();
    expect(msg({ locks: [] })).toBeNull();
    expect(msg({ locks: [{ until: '1800000000000', key: maker, extra: 1 }] })).toBeNull();
    // never with a creation
    expect(msg({ locks: [{ until: '1800000000000', key: maker }], prep: p.prep })).toBeNull();
  });
});
