// Token facts from the chain (KCC-23): a declaration is used only after its launch proof verified against the covenant id asked for; the display
// prefers it to the registry's snapshot and flags any disagreement; an open-list token takes its scale from it.
import { beforeEach, describe, expect, it } from 'vitest';
import {
  adoptDeclaration, declarationConflicts, declaredToken, loadDeclaration, resetDeclarationsForTests, restoreDeclarations, subscribeDeclarations,
} from './token-declarations';
import { decimalsNote, displayToken } from '../app/token-display';
import { synthesizeOpenToken } from '../kob/open-token';
import { emptyIssueForm, planIssue, type IssuePlan } from '../kob/issue';
import { loadKobNode } from '../kob/wasm.node';
import { pubkeyOf } from '../testing/local-signer';
import type { TokenInfo } from '../kob/registry';
import type { IndexerTokenView } from './indexer-types';

const kob = loadKobNode();
const NET = 'testnet-10';
const maker = pubkeyOf('33'.repeat(32));
const mem = () => {
  const m = new Map<string, string>();
  return { m, getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
};
const create = (decimals = 2, ticker = 'DECL', name = 'Declared Coin'): IssuePlan =>
  planIssue(kob, { ...emptyIssueForm(), name, ticker, decimals, supply: '1000', description: 'on chain' }, {
    funding: [{ transactionId: '0c'.repeat(32), index: 0, amount: (500n * 100_000_000n).toString(), blockDaaScore: '1', pubkey: maker }],
    maker,
    network: NET,
  });

beforeEach(() => resetDeclarationsForTests());

describe('declarations: verified before use', () => {
  it('a launch proof is adopted for its own covenant id only; a forged or misattributed one is not', () => {
    const p = create();
    const id = p.token.covenantId;
    const s = mem();
    let calls = 0;
    const off = subscribeDeclarations(() => calls++);
    expect(adoptDeclaration(kob, NET, 'ff'.repeat(32), p.docs.launchProof, s)).toBeNull(); // claimed for another token
    const forged = JSON.parse(JSON.stringify(p.docs.launchProof));
    forged.preparation.payload = `00${Buffer.from('{"name":"Declared Coin","symbol":"DECL","decimals":8}').toString('hex')}`;
    expect(adoptDeclaration(kob, NET, id, forged, s)).toBeNull();
    expect(declaredToken(NET, id)).toBeUndefined();
    const d = adoptDeclaration(kob, NET, id, p.docs.launchProof, s)!;
    expect(d).toMatchObject({ covenantId: id, name: 'Declared Coin', symbol: 'DECL', decimals: 2, description: 'on chain', preparationTxid: p.prep.tx.id });
    expect(declaredToken(NET, id)).toEqual(d);
    expect(declaredToken('mainnet', id)).toBeUndefined();
    expect(calls).toBe(1);
    off();
    // kept as a proof, re-verified when read back
    resetDeclarationsForTests();
    expect(declaredToken(NET, id)).toBeUndefined();
    restoreDeclarations(kob, s);
    expect(declaredToken(NET, id)).toMatchObject({ decimals: 2 });
  });

  it('looks a token up once: a proof is verified, none is remembered for a while, a failed read is not', async () => {
    const p = create();
    const id = p.token.covenantId;
    const asked: string[] = [];
    let answer: 'proof' | 'none' | 'fail' | 'forged' = 'fail';
    const source = {
      declaration: async (cov: string) => {
        asked.push(cov);
        if (answer === 'fail') throw new Error('down');
        if (answer === 'none') return null;
        const proof = JSON.parse(JSON.stringify(p.docs.launchProof));
        if (answer === 'forged') proof.covenantId = 'ee'.repeat(32);
        return proof;
      },
    };
    expect(await loadDeclaration(kob, source, NET, id, null)).toBeNull();
    answer = 'none';
    expect(await loadDeclaration(kob, source, NET, id, null)).toBeNull(); // the failure was not remembered: asked again
    expect(await loadDeclaration(kob, source, NET, id, null)).toBeNull(); // "none" is: not asked again for a while
    expect(asked).toHaveLength(2);
    resetDeclarationsForTests();
    answer = 'forged';
    expect(await loadDeclaration(kob, source, NET, id, null)).toBeNull();
    expect(declaredToken(NET, id)).toBeNull();
    resetDeclarationsForTests();
    answer = 'proof';
    const [a, b] = await Promise.all([loadDeclaration(kob, source, NET, id, null), loadDeclaration(kob, source, NET, id, null)]);
    expect(a).toEqual(b);
    expect(a?.decimals).toBe(2);
    expect(await loadDeclaration(kob, null, NET, 'aa'.repeat(32), null)).toBeNull(); // no source
  });
});

describe('declarations in the display (facts from the chain first, the registry is curation)', () => {
  const reg = (entry: Partial<TokenInfo> & { covenantId: string }) => ({ byCovenantId: new Map([[entry.covenantId, { ticker: 'DECL', name: 'Declared Coin', decimals: 2, ...entry } as TokenInfo]]) });

  it('an unlisted token is shown on its declaration, marked as declared on chain', () => {
    const p = create(5, 'ONCH', 'On Chain');
    adoptDeclaration(kob, NET, p.token.covenantId, p.docs.launchProof, null, false);
    const tk = displayToken({ byCovenantId: new Map() }, NET, p.token.covenantId, mem())!;
    expect(tk).toMatchObject({ ticker: 'ONCH', name: 'On Chain', decimals: 5, decimalsSource: 'declared', tradable: false });
    expect(decimalsNote(tk)).toMatchObject({ text: 'declared on chain' });
    // a market whose price scale is not the declared decimals: flagged
    const off = displayToken({ byCovenantId: new Map() }, NET, p.token.covenantId, mem(), 1000n)!;
    expect(off.declarationConflicts).toEqual(['decimals']);
    expect(decimalsNote(off)).toMatchObject({ text: 'facts disagree' });
    expect(displayToken({ byCovenantId: new Map() }, NET, p.token.covenantId, mem(), 100_000n)!.declarationConflicts).toBeUndefined();
  });

  it('a registry entry that agrees is shown as it is; one that disagrees shows the on-chain values, flagged, never silently the registry', () => {
    const p = create();
    const id = p.token.covenantId;
    adoptDeclaration(kob, NET, id, p.docs.launchProof, null, false);
    const agree = displayToken(reg({ covenantId: id }), NET, id)!;
    expect(agree.declarationConflicts).toBeUndefined();
    expect(decimalsNote(agree)).toBeNull();
    const lying = displayToken(reg({ covenantId: id, decimals: 8, ticker: 'DECLX' }), NET, id)!;
    expect(lying).toMatchObject({ decimals: 2, ticker: 'DECL', declarationConflicts: ['decimals', 'symbol'] });
    expect(decimalsNote(lying)!.title).toMatch(/decimals, ticker/);
    expect(declarationConflicts({ ticker: 'DECL', name: 'Other', decimals: 2 }, declaredToken(NET, id))).toEqual(['name']);
    expect(declarationConflicts({ ticker: 'X', name: 'Y', decimals: 9 }, null)).toEqual([]);
  });

  it('an open-list token takes its scale from its declared decimals; orders of another scale are a conflict', () => {
    const kcc = kob.templates().find((t) => t.name === 'KCC20Ref')!;
    const view = (over: Partial<IndexerTokenView> = {}): IndexerTokenView => ({
      ticker: '', covenant_id: 'ab'.repeat(32), template_hash: kcc.hash, extension_commitment: '00'.repeat(32), scale: null, decimals: null, open_asks: 0, open_bids: 0,
      standing: 'unverified', powers: [], ...over,
    });
    const declared = { decimals: 4, symbol: 'OPEN', name: 'Open Coin' };
    // no order yet: the declaration sets the scale, no entered decimals needed
    const r = synthesizeOpenToken(kob, view(), [], null, null, [], { network: NET, declared });
    expect(r.ok && r.info).toMatchObject({ decimals: 4, ticker: 'OPEN', name: 'Open Coin', openScale: 'declared' });
    // the market's orders use the declared scale: fine; another: a conflict, whatever was entered
    expect(synthesizeOpenToken(kob, view(), [{ scale: 10_000 }], null, null, [], { network: NET, declared }).ok).toBe(true);
    expect(synthesizeOpenToken(kob, view({ market_scale: 100 }), [], null, null, [], { network: NET, declared, decimals: 2 })).toEqual({ ok: false, reason: 'scale-conflict' });
    // more than 9 decimals: the scale stops at 10^9 (as for a registry token), the decimals stay the declared ones
    const big = synthesizeOpenToken(kob, view(), [], null, null, [], { network: NET, declared: { ...declared, decimals: 12 } });
    expect(big.ok && big.info.decimals).toBe(12);
    expect(synthesizeOpenToken(kob, view({ market_scale: 1_000_000_000 }), [], null, null, [], { network: NET, declared: { ...declared, decimals: 12 } }).ok).toBe(true);
  });
});
