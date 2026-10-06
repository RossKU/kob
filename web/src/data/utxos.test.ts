import { describe, expect, it } from 'vitest';
import { COINBASE_MATURITY_DAA, createUtxoService, isSpendableFunding, p2pkScriptPublicKey } from './utxos';
import { loadKaspaSdkNode } from './kaspa-sdk.node';
import { pubkeyToAddress } from './kaspa-sdk';
import { MockNode } from '../testing/mock-node';
import { MAKER_PK, OTHER_PK } from '../testing/token-fixtures';
import type { NodeUtxo } from './node';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';
const addr = (pk: string) => pubkeyToAddress(sdk, pk, NET);

const u = (over: Partial<NodeUtxo> & { n: number }): NodeUtxo => ({
  address: addr(MAKER_PK),
  transactionId: over.n.toString(16).padStart(2, '0').repeat(32),
  index: 0,
  amount: '100000000',
  scriptPublicKey: p2pkScriptPublicKey(MAKER_PK),
  blockDaaScore: '1000',
  isCoinbase: false,
  covenantId: null,
  ...over,
});

describe('createUtxoService.fundingFor', () => {
  it('returns the key\'s plain UTXOs as KeyUtxo, largest first, ties by outpoint', async () => {
    const node = new MockNode({ daa: 1_000_000n });
    node.setUtxos([u({ n: 3, amount: '5' }), u({ n: 1, amount: '900' }), u({ n: 2, amount: '900' }), u({ n: 2, index: 1, amount: '900' })]);
    const svc = createUtxoService({ node, sdk, network: NET });
    const out = await svc.fundingFor(MAKER_PK);
    expect(out.map((k) => [k.transactionId.slice(0, 2), k.index, k.amount])).toEqual([['01', 0, '900'], ['02', 0, '900'], ['02', 1, '900'], ['03', 0, '5']]);
    expect(out[0]).toEqual({ transactionId: '01'.repeat(32), index: 0, amount: '900', blockDaaScore: '1000', covenantId: null, pubkey: MAKER_PK });
    expect(node.utxoQueries).toEqual([[addr(MAKER_PK)]]);
  });

  it('only asks the node for the key\'s own address (other keys\' UTXOs never appear)', async () => {
    const node = new MockNode();
    node.setUtxos([u({ n: 1 }), u({ n: 2, address: addr(OTHER_PK), scriptPublicKey: p2pkScriptPublicKey(OTHER_PK) })]);
    const out = await createUtxoService({ node, sdk, network: NET }).fundingFor(MAKER_PK);
    expect(out.map((k) => k.transactionId.slice(0, 2))).toEqual(['01']);
  });

  it('skips covenant UTXOs (tokens, orders) and scripts that are not the key\'s P2PK', async () => {
    const node = new MockNode();
    node.setUtxos([u({ n: 1 }), u({ n: 2, covenantId: '77'.repeat(32) }), u({ n: 3, scriptPublicKey: '0000aa20' + 'ab'.repeat(32) + '87' })]);
    const out = await createUtxoService({ node, sdk, network: NET }).fundingFor(MAKER_PK);
    expect(out.map((k) => k.transactionId.slice(0, 2))).toEqual(['01']);
  });

  it('coinbase maturity: spendable from exactly 1000 DAA after its block', async () => {
    const node = new MockNode({ daa: 10_000n });
    node.setUtxos([
      u({ n: 1, isCoinbase: true, blockDaaScore: '9001' }), // 999 old: immature
      u({ n: 2, isCoinbase: true, blockDaaScore: '9000' }), // 1000 old: mature
      u({ n: 3, isCoinbase: true, blockDaaScore: '1' }),
      u({ n: 4, isCoinbase: false, blockDaaScore: '9999' }), // not a coinbase: any age
    ]);
    const out = await createUtxoService({ node, sdk, network: NET }).fundingFor(MAKER_PK);
    expect(out.map((k) => k.transactionId.slice(0, 2)).sort()).toEqual(['02', '03', '04']);
    expect(COINBASE_MATURITY_DAA).toBe(1000n);
  });

  it('reads the clock only when a coinbase UTXO is present', async () => {
    let clockCalls = 0;
    const node = new MockNode();
    const orig = node.getClock.bind(node);
    node.getClock = async () => { clockCalls++; return orig(); };
    node.setUtxos([u({ n: 1 })]);
    const svc = createUtxoService({ node, sdk, network: NET });
    await svc.fundingFor(MAKER_PK);
    expect(clockCalls).toBe(0);
    node.addUtxo(u({ n: 2, isCoinbase: true }));
    await svc.fundingFor(MAKER_PK);
    expect(clockCalls).toBe(1);
  });

  it('keeps amounts above 2^53 exact and sorts them correctly', async () => {
    const node = new MockNode();
    node.setUtxos([u({ n: 1, amount: '9007199254740992' }), u({ n: 2, amount: '9007199254740993' })]);
    const out = await createUtxoService({ node, sdk, network: NET }).fundingFor(MAKER_PK);
    expect(out.map((k) => k.amount)).toEqual(['9007199254740993', '9007199254740992']);
  });

  it('propagates node failures and rejects a malformed key', async () => {
    const node = new MockNode().setUnavailable(true);
    const svc = createUtxoService({ node, sdk, network: NET });
    await expect(svc.fundingFor(MAKER_PK)).rejects.toMatchObject({ code: 'unavailable' });
    await expect(svc.fundingFor('abcd')).rejects.toThrow(/32-byte/);
  });

  it('isSpendableFunding treats an unreported script as unknown (in-memory mocks) and a null clock as immature-for-coinbase', () => {
    expect(isSpendableFunding(u({ n: 1, scriptPublicKey: '' }), MAKER_PK, null)).toBe(true);
    expect(isSpendableFunding(u({ n: 1, isCoinbase: true }), MAKER_PK, null)).toBe(false);
  });
});
