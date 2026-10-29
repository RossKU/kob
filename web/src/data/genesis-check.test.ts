// KOB's own genesis check (data/genesis-check.ts) over RECORDED chain data: the genesis groups of real mainnet and testnet-10 tokens as kascov.io and the Kaspa
// REST explorers served them on 2026-10-28 (src/testing/genesis-chain.json; the same groups with their verdicts are the Rust vectors, crates/kob-protocol/
// vectors/genesis-open.json). Nothing here reaches the network: every fetch is answered from the recording, anything else throws. The verification runs in the
// real kob-wasm (`verifyGenesis`), so a pass is a pass of the Rust rules.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { createGenesisChecker, EXPLORER_BASE, lastPush, parseExplorerGenesis, parseKascovGenesis, revealFromSpend, type GenesisCheck, type GenesisChecker } from './genesis-check';
import { verifyGenesis, type GenesisRequest } from '../kob/genesis';
import { loadKobNode } from '../kob/wasm.node';

interface Case {
  name: string;
  network: 'mainnet' | 'testnet-10';
  ticker: string | null;
  covenantId: string;
  kascovStatus: string;
  viewTemplateHash: string | null;
  kascov: Record<string, unknown> & { genesis_txid: string; utxos: { outpoint: string; value: number; script_hex: string; live: boolean; revealed_hex?: string; spent_txid?: string }[] };
  explorerTx: Record<string, unknown> | null;
}
const chain = JSON.parse(readFileSync(new URL('../testing/genesis-chain.json', import.meta.url), 'utf8')) as { cases: Case[]; spends: Record<string, unknown> };
const vectors = JSON.parse(readFileSync(new URL('../../../crates/kob-protocol/vectors/genesis-open.json', import.meta.url), 'utf8')) as {
  cases: { name: string; request: GenesisRequest; expect: Record<string, unknown> }[];
};
const kob = loadKobNode();
const BASE = 'https://kascov.io/data';
const caseOf = (name: string): Case => chain.cases.find((c) => c.name === name)!;
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

interface Net {
  /** every URL asked */
  urls: string[];
  fetch: typeof fetch;
}

/** A fetch that answers from the recording of one case (`kascov`, `explorerTx`, `spends`, `addressPages`) and throws on anything else. */
function recorded(c: Case, o: { kascov?: unknown; explorerTx?: unknown; kascovStatus?: number; explorerStatus?: number; spends?: Record<string, unknown>; addressPages?: Record<string, unknown[]>; offline?: boolean } = {}): Net {
  const urls: string[] = [];
  const json = (d: unknown, status = 200) => new Response(JSON.stringify(d), { status, headers: { 'content-type': 'application/json' } });
  const f = (async (input: string) => {
    const u = String(input);
    urls.push(u);
    if (o.offline) throw new TypeError('offline');
    if (u === `${BASE}/${c.network}/c/${c.covenantId}.json`) return o.kascovStatus ? json({}, o.kascovStatus) : json(o.kascov ?? c.kascov);
    const tx = /^(https:\/\/api(?:-tn10)?\.kaspa\.org)\/transactions\/([0-9a-f]{64})\?/.exec(u);
    if (tx) {
      expect(tx[1], 'the explorer of the token\'s network').toBe(EXPLORER_BASE[c.network]);
      if (tx[2] === c.kascov.genesis_txid) {
        const doc = o.explorerTx === undefined ? c.explorerTx : o.explorerTx;
        return o.explorerStatus || !doc ? json({}, o.explorerStatus ?? 404) : json(doc);
      }
      const spend = (o.spends ?? chain.spends)[tx[2]!];
      return spend ? json(spend) : json({}, 404);
    }
    const addr = /\/addresses\/([^/]+)\/full-transactions\?limit=50&offset=(\d+)&/.exec(u);
    if (addr) return json(o.addressPages?.[addr[1]!]?.slice(Number(addr[2]), Number(addr[2]) + 50) ?? []);
    throw new Error(`a test must not reach ${u}`);
  }) as unknown as typeof fetch;
  return { urls, fetch: f };
}

const checkerFor = (net: Net, extra: { storage?: Pick<Storage, 'getItem' | 'setItem'> | null; now?: () => number; verify?: (r: GenesisRequest) => ReturnType<typeof verifyGenesis> } = {}): GenesisChecker =>
  createGenesisChecker({ verify: extra.verify ?? ((r) => verifyGenesis(kob, r)), fetch: net.fetch, storage: extra.storage ?? null, ...(extra.now ? { now: extra.now } : {}) });
const run = (ch: GenesisChecker, c: Case): Promise<GenesisCheck> => ch.check(c.network, c.covenantId, { kascovBase: BASE, programHint: c.viewTemplateHash });
const memStorage = (): Pick<Storage, 'getItem' | 'setItem'> & { data: Map<string, string> } => {
  const data = new Map<string, string>();
  return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v) };
};

describe('the recording', () => {
  it('holds a mainnet and a testnet-10 token set, each token of which also has its Rust vector', () => {
    expect(new Set(chain.cases.map((c) => c.network))).toEqual(new Set(['mainnet', 'testnet-10']));
    expect(chain.cases.map((c) => c.name)).toEqual(vectors.cases.map((c) => c.name));
  });
});

describe('verifying recorded tokens', () => {
  it('a testnet-10 token with a KCC-23 declaration (BRW) verifies from kascov\'s file alone: one request, KOB\'s own verdict', async () => {
    const c = caseOf('brw');
    const net = recorded(c);
    const r = await run(checkerFor(net), c);
    expect(r).toMatchObject({ status: 'verified', source: 'kascov', program: 'KCC20PublicMint', supply: 1_000_000n, mintAllowance: null });
    expect(net.urls).toEqual([`${BASE}/testnet-10/c/${c.covenantId}.json`]);
  });

  it('a mainnet public-mint token with an unspent TokenSeed is rebuilt and verifies', async () => {
    const c = caseOf('wiggle-publicmint-seed');
    const r = await run(checkerFor(recorded(c)), c);
    expect(r).toMatchObject({ status: 'verified', source: 'kascov', program: 'KCC20PublicMint' });
    // the recorded verdict names the rebuilt seed
    expect(vectors.cases.find((v) => v.name === c.name)!.expect.rebuilt).toHaveLength(1);
  });

  it('a mainnet KaspaRocket token (no declaration, so no authorising outpoint in kascov\'s file) takes it from the explorer: two requests', async () => {
    const c = caseOf('kota-rocket');
    const net = recorded(c);
    const r = await run(checkerFor(net), c);
    expect(r).toMatchObject({ status: 'verified', source: 'explorer', program: 'KaspaRocketToken11458', supply: 1_000_000_000n });
    expect(net.urls).toHaveLength(2);
    expect(net.urls[1]).toMatch(/^https:\/\/api\.kaspa\.org\/transactions\//);
  });

  it('a public minter\'s allowance is reported: the most that could still be minted, and whether the genesis minter has been used', async () => {
    const c = caseOf('kqa-publicmint-seed');
    const r = await run(checkerFor(recorded(c)), c);
    // its genesis minter was spent since: some of the 100000 may be minted already, KOB does not say how much is left
    expect(r).toMatchObject({ status: 'verified', mintAllowance: 100_000n, mintersUnspent: false });
    // the same group with the minter unspent (as kascov would then say): the whole allowance is there
    const doc = clone(c.kascov);
    for (const u of doc.utxos) {
      if (u.revealed_hex) {
        u.live = true;
        delete u.spent_txid;
      }
    }
    const live = await run(checkerFor(recorded(c, { kascov: doc })), c);
    expect(live).toMatchObject({ status: 'verified', mintAllowance: 100_000n, mintersUnspent: true });
    // a token without a public minter has no allowance to report
    expect(await run(checkerFor(recorded(caseOf('brw'))), caseOf('brw'))).toMatchObject({ mintAllowance: null, mintersUnspent: null });
  });

  it('a KRON minter is unbounded: no allowance of 0 is reported for it (audit 2026-10-29 F7)', async () => {
    const c = caseOf('brw');
    const asKron = (r: GenesisRequest) => ({ ...verifyGenesis(kob, r), program: 'KronToken2433', minterOutputs: [0], mintAllowance: 0n }) as ReturnType<typeof verifyGenesis>;
    const r = await run(checkerFor(recorded(c), { verify: asKron }), c);
    expect(r).toMatchObject({ status: 'verified', mintAllowance: null, mintUnbounded: true });
  });

  it('every recorded group yields exactly its Rust vector\'s request and verdict', async () => {
    for (const v of vectors.cases) {
      const c = caseOf(v.name);
      const sent: GenesisRequest[] = [];
      const ch = checkerFor(recorded(c), { verify: (r) => (sent.push(r), verifyGenesis(kob, r)) });
      await run(ch, c);
      expect(sent.at(-1), v.name).toEqual(v.request);
      const r = verifyGenesis(kob, v.request);
      expect(r.status, v.name).toBe(v.expect.status);
    }
  });
});

describe('failing and unavailable', () => {
  it('a hidden output fails the check as contaminated, although kascov marks the token verified or not', async () => {
    const c = caseOf('kusdt-contaminated');
    const r = await run(checkerFor(recorded(c)), c);
    expect(r).toMatchObject({ status: 'failed', code: 'contaminated' });
    expect((r as { message: string }).message).toContain('not an instance of the token program');
    // the same with kascov saying verified in its list: the check does not read it
    const doc = clone(c.kascov);
    (doc as Record<string, unknown>).status = 'verified';
    expect(await run(checkerFor(recorded(c, { kascov: doc })), c)).toMatchObject({ status: 'failed', code: 'contaminated' });
  });

  it('a token whose genesis holds an unspent output of a program nobody revealed (a launchpad curve cell) stays unavailable (history not available)', async () => {
    const c = caseOf('dawdaw-unspent-curve');
    const net = recorded(c);
    expect(await run(checkerFor(net), c)).toEqual({ status: 'unavailable', reason: 'history' });
    // the explorer was asked for the genesis transaction (its keys are owner candidates for a seed) and for nothing it could not use
    expect(net.urls.some((u) => u.includes(`/transactions/${c.kascov.genesis_txid}`))).toBe(true);
  });

  it('data that does not match the covenant id fails; a lying kascov does not defeat an honest explorer', async () => {
    const c = caseOf('brw');
    const lie = clone(c.kascov);
    lie.utxos[0]!.value += 1;
    // explorer honest: the check recovers from the explorer's own bytes and verifies
    expect(await run(checkerFor(recorded(c, { kascov: lie })), c)).toMatchObject({ status: 'verified', source: 'explorer' });
    // explorer down: a mismatch only kascov's file shows is a supplier's word, not a verdict (a stale or hostile file must not brand a clean token for the
    // session, audit 2026-10-29 F8): unavailable, asked again later
    expect(await run(checkerFor(recorded(c, { kascov: lie, explorerStatus: 503 })), c)).toMatchObject({ status: 'unavailable', reason: 'unreachable' });
    // both lie
    const tx = clone(c.explorerTx) as { outputs: { amount: number; covenant_id: string | null }[] };
    tx.outputs.find((o) => o.covenant_id)!.amount += 1;
    expect(await run(checkerFor(recorded(c, { kascov: lie, explorerTx: tx })), c)).toMatchObject({ status: 'failed', code: 'mismatch' });
    // a script byte flipped in kascov's file: it no longer hashes to its output (explorer down: unavailable, as above)
    const flip = clone(c.kascov);
    const s = flip.utxos[0]!.revealed_hex!;
    flip.utxos[0]!.revealed_hex = `${s.slice(0, 500)}${s.slice(500, 502) === '00' ? '01' : '00'}${s.slice(502)}`;
    expect(await run(checkerFor(recorded(c, { kascov: flip, explorerStatus: 503 })), c)).toMatchObject({ status: 'unavailable' });
  });

  it('an explorer transaction that was changed fails (the Rocket token, whose group comes from the explorer)', async () => {
    const c = caseOf('kota-rocket');
    const tx = clone(c.explorerTx) as { inputs: { previous_outpoint_index: string }[] };
    tx.inputs[0]!.previous_outpoint_index = '9';
    expect(await run(checkerFor(recorded(c, { explorerTx: tx })), c)).toMatchObject({ status: 'failed', code: 'mismatch' });
  });

  it('unreachable or empty sources are unavailable, never a verdict on the token', async () => {
    const c = caseOf('brw');
    expect(await run(checkerFor(recorded(c, { offline: true })), c)).toEqual({ status: 'unavailable', reason: 'unreachable' });
    expect(await run(checkerFor(recorded(c, { kascovStatus: 404 })), c)).toEqual({ status: 'unavailable', reason: 'history' });
    expect(await run(checkerFor(recorded(c, { kascovStatus: 503 })), c)).toEqual({ status: 'unavailable', reason: 'unreachable' });
    // a file for another covenant, a changed shape
    expect(await run(checkerFor(recorded(c, { kascov: { ...clone(c.kascov), covenant_id: 'ab'.repeat(32) } })), c)).toEqual({ status: 'unavailable', reason: 'history' });
    expect(await run(checkerFor(recorded(c, { kascov: { tokens: [] } })), c)).toEqual({ status: 'unavailable', reason: 'history' });
    // no kascov (turned off), a network kascov does not serve
    expect(await checkerFor(recorded(c)).check(c.network, c.covenantId, { kascovBase: '' })).toEqual({ status: 'unavailable', reason: 'history' });
    expect(await checkerFor(recorded(c)).check('devnet', c.covenantId, { kascovBase: BASE })).toEqual({ status: 'unavailable', reason: 'history' });
    // a file without the authorising outpoint and an explorer that does not have the transaction
    const k = clone(c.kascov);
    delete (k as Record<string, unknown>).kcc23;
    expect(await run(checkerFor(recorded(c, { kascov: k, explorerStatus: 404 })), c)).toEqual({ status: 'unavailable', reason: 'history' });
    expect(await run(checkerFor(recorded(c, { kascov: k, explorerStatus: 503 })), c)).toEqual({ status: 'unavailable', reason: 'unreachable' });
  });

  it('a verify that throws (bindings of another build) is unavailable, not a crash', async () => {
    const c = caseOf('brw');
    const r = await run(checkerFor(recorded(c, { explorerStatus: 404 }), { verify: () => { throw new Error('boom'); } }), c);
    expect(r).toEqual({ status: 'unavailable', reason: 'history' });
  });
});

describe('finding the scripts that spends revealed', () => {
  it('the last push of a spend\'s signature script is the redeem script kascov also gives', () => {
    const c = caseOf('brw');
    const spend = Object.values(chain.spends)[0] as { transaction_id: string; inputs: { signature_script: string }[] };
    expect(lastPush(spend.inputs[0]!.signature_script)).toBe(c.kascov.utxos[0]!.revealed_hex);
    expect(revealFromSpend(spend, spend.transaction_id, c.kascov.genesis_txid, 0)).toBe(c.kascov.utxos[0]!.revealed_hex);
    // not that spend, not that outpoint, not that transaction
    expect(revealFromSpend(spend, spend.transaction_id, c.kascov.genesis_txid, 1)).toBeNull();
    expect(revealFromSpend(spend, 'ab'.repeat(32), c.kascov.genesis_txid, 0)).toBeNull();
    expect(lastPush('')).toBeNull();
    expect(lastPush('00')).toBeNull();
    expect(lastPush('zz')).toBeNull();
    // an opcode that is not a push; a push that runs past the end
    expect(lastPush('76')).toBeNull();
    expect(lastPush('4c05aabb')).toBeNull();
    // direct, PUSHDATA1 and PUSHDATA2 pushes: the last one wins
    expect(lastPush('02aabb')).toBe('aabb');
    expect(lastPush(`4c03aabbcc`)).toBe('aabbcc');
    expect(lastPush(`4d0300aabbcc`)).toBe('aabbcc');
    expect(lastPush('01aa02bbcc')).toBe('bbcc');
  });

  it('a busy token\'s file lacks its genesis outputs: the group comes from the explorer and the script from the history of its address', async () => {
    const c = caseOf('brw');
    const truncated = clone(c.kascov);
    truncated.utxos = [];
    (truncated as Record<string, unknown>).utxos_truncated = true;
    const tx = c.explorerTx as { outputs: { script_public_key_address: string; covenant_id: string | null }[] };
    const address = tx.outputs.find((o) => o.covenant_id)!.script_public_key_address;
    const net = recorded(c, { kascov: truncated, addressPages: { [address]: [{ transaction_id: 'cd'.repeat(32), inputs: [] }, ...Object.values(chain.spends)] } });
    const r = await run(checkerFor(net), c);
    expect(r).toMatchObject({ status: 'verified', source: 'explorer' });
    expect(net.urls.some((u) => u.includes(`/addresses/${address}/full-transactions`))).toBe(true);
  });

  it('a spend kascov names is fetched once for a script it did not give', async () => {
    const c = caseOf('brw');
    const bare = clone(c.kascov);
    delete bare.utxos[0]!.revealed_hex;
    bare.utxos[0]!.spent_txid = Object.keys(chain.spends)[0]!;
    const k = bare as Record<string, unknown>;
    delete k.kcc23; // no authorising outpoint in the file: the explorer is needed, and the spend with it
    const net = recorded(c, { kascov: bare });
    expect(await run(checkerFor(net), c)).toMatchObject({ status: 'verified', source: 'explorer' });
    expect(net.urls.filter((u) => u.includes(Object.keys(chain.spends)[0]!))).toHaveLength(1);
  });
});

describe('parsers', () => {
  const c = caseOf('brw');
  it('kascov\'s file: only the outputs of the genesis transaction, for the right covenant and network', () => {
    const g = parseKascovGenesis(c.kascov, c.covenantId, 'testnet-10')!;
    expect(g.genesisTxid).toBe(c.kascov.genesis_txid);
    expect(g.outputs.map((o) => o.index)).toEqual([0]);
    expect(g.authorizing).toEqual({ txid: '0b197619e4924be7845353771d8a86f3bfc72199ccd63ad4275d98d751930b43', index: 0 });
    expect(g.partial).toBe(false);
    expect(parseKascovGenesis(c.kascov, c.covenantId, 'mainnet')).toBeNull();
    expect(parseKascovGenesis(c.kascov, 'ab'.repeat(32), 'testnet-10')).toBeNull();
    expect(parseKascovGenesis({ ...c.kascov, genesis_txid: 'zz' }, c.covenantId, 'testnet-10')).toBeNull();
    // a value that is not a safe integer, a script that is not hex: unusable
    const bad = clone(c.kascov);
    bad.utxos[0]!.script_hex = 'xyz';
    expect(parseKascovGenesis(bad, c.covenantId, 'testnet-10')).toBeNull();
    // a declaration that kascov did not verify gives no authorising outpoint
    expect(parseKascovGenesis({ ...c.kascov, kcc23: { binding: 'unbound', preparation_txid: 'ab'.repeat(32), output_index: 0 } }, c.covenantId, 'testnet-10')!.authorizing).toBeNull();
    // truncated lists are said to be partial
    expect(parseKascovGenesis({ ...c.kascov, reveal_bodies_omitted: 2 }, c.covenantId, 'testnet-10')!.partial).toBe(true);
  });

  it('the explorer\'s transaction: the group, the authorising input and the keys it pays to', () => {
    const e = parseExplorerGenesis(c.explorerTx, c.kascov.genesis_txid, c.covenantId)!;
    expect(e.outputs.map((o) => [o.index, o.value])).toEqual([[0, '200000000']]);
    expect(e.authorizing).toEqual({ txid: '0b197619e4924be7845353771d8a86f3bfc72199ccd63ad4275d98d751930b43', index: 0 });
    expect(e.ownerHints).toEqual(['2325261dbd940efb4be2d3774b869c46c16a8f47e3c417da84e74e1f88ab6191']);
    expect(parseExplorerGenesis(c.explorerTx, 'ab'.repeat(32), c.covenantId)).toBeNull();
    expect(parseExplorerGenesis({ ...(c.explorerTx as object), outputs: 'x' }, c.kascov.genesis_txid, c.covenantId)).toBeNull();
  });
});

describe('once per token per session', () => {
  it('asks the sources once, then answers from memory; an unavailable answer is asked again only after the retry time', async () => {
    const c = caseOf('brw');
    const net = recorded(c);
    const ch = checkerFor(net);
    const a = await run(ch, c);
    const b = await run(ch, c);
    expect(b).toBe(a);
    expect(net.urls).toHaveLength(1);
    expect(ch.known(c.network, c.covenantId)).toBe(a);
    // concurrent callers share one run
    const net2 = recorded(c);
    const ch2 = checkerFor(net2);
    await Promise.all([run(ch2, c), run(ch2, c), run(ch2, c)]);
    expect(net2.urls).toHaveLength(1);
    // unavailable: kept for the retry time, then asked again
    let clock = 0;
    const down = recorded(c, { offline: true });
    const ch3 = checkerFor(down, { now: () => clock });
    expect((await run(ch3, c)).status).toBe('unavailable');
    expect((await run(ch3, c)).status).toBe('unavailable');
    expect(down.urls).toHaveLength(1);
    clock += 301_000;
    await run(ch3, c);
    expect(down.urls).toHaveLength(2);
  });

  it('a saved copy of the evidence is verified again before it is used, and a tampered copy is ignored', async () => {
    const c = caseOf('brw');
    const store = memStorage();
    expect((await run(checkerFor(recorded(c), { storage: store }), c)).status).toBe('verified');
    expect(store.data.size).toBe(1);
    // a new session, no network: the saved evidence verifies again (source: saved)
    const offline = recorded(c, { offline: true });
    expect(await run(checkerFor(offline, { storage: store }), c)).toMatchObject({ status: 'verified', source: 'saved', supply: 1_000_000n });
    expect(offline.urls).toHaveLength(0);
    // a saved copy that was altered does not verify and is not trusted: the sources are asked
    const key = [...store.data.keys()][0]!;
    store.data.set(key, store.data.get(key)!.replace('"value":"200000000"', '"value":"200000001"'));
    const again = recorded(c, { offline: true });
    expect(await run(checkerFor(again, { storage: store }), c)).toEqual({ status: 'unavailable', reason: 'unreachable' });
    expect(again.urls).toHaveLength(1);
    // no storage at all, or storage that throws: the check just runs
    const broken = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); } };
    expect((await run(checkerFor(recorded(c), { storage: broken }), c)).status).toBe('verified');
    // only a token that verified is saved
    const s2 = memStorage();
    await run(checkerFor(recorded(caseOf('kusdt-contaminated')), { storage: s2 }), caseOf('kusdt-contaminated'));
    expect(s2.data.size).toBe(0);
  });
});
