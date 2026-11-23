// R3A-1 (web half) and R3A-2: a node-only resolve of a Collected (mode 1) order after a partial fill
// finds the order and its bought-custody, and its cancel returns both; an unbound curve after its
// first fill is found under its record's token. Every cancel is signed and validated in the script
// engine and passes the pre-sign decoder.
import { describe, expect, it } from 'vitest';
import { planCancel, snapshotFromRecord, type CancelEnv, type CancelPlan } from './cancel';
import { decodeSigning } from './decode';
import { recordsFromBuilt, resolveRecord, type PlacementRecord } from './records';
import { custodyState } from './token-state';
import type { Hex, OrderState, TokenProgram } from './types';
import { loadKobNode } from './wasm.node';
import { FakeChain, MAKER, keyUtxo, nodeFactsOf, placeGolden, signAndValidate, testAddress } from '../testing/chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
const opts = { maker: MAKER.pk, network: 'testnet-10', placedAtUnix: 1_790_000_000n, placedAtDaa: 1000n };
const env = (): CancelEnv => ({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * KAS, 201)], tokenUtxos: [], clock: { daa: 2000n } });
const programOf = (hash: Hex): TokenProgram => kob.templates().find((t) => t.hash === hash)!.name as TokenProgram;
const F = (o: OrderState): Record<string, string> => o.state as unknown as Record<string, string>;
const TX = 'ab'.repeat(32);

/** The golden placement `name` as a record whose state is `edit` of the placed one (a Collected order). */
function collectedRecord(name: string, edit: Record<string, string>): { rec: PlacementRecord; st: OrderState } {
  const p = placeGolden(kob, name);
  const rec = recordsFromBuilt(kob, p.built, opts)[0]!;
  const st0 = kob.decodeState(rec.kind as never, rec.state) as OrderState;
  const st = { ...st0, state: { ...st0.state, mode: '1', recvThreshold: '0', topupTip: '0', ...edit } } as OrderState;
  return { rec: { ...rec, state: kob.encodeState(st) }, st };
}

/** Puts the continuation `cont` on the chain with its bought-custody (and, for a pair sell, its custody of A). */
function chainWith(rec: PlacementRecord, cont: OrderState, opts2: { custody?: { tpl: Hex; token: Hex; ext: Hex; amount: bigint } } = {}): FakeChain {
  const s = F(cont);
  const chain = new FakeChain();
  chain.add({ transactionId: TX, index: 0, amount: (20n * KAS).toString(), scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId, blockDaaScore: '1500' });
  const pair = 'tCovId' in s;
  const tpl = pair ? s.tTplHash : s.tokenTplHash;
  const token = pair ? s.tCovId : s.tokenCovId;
  const ext = pair ? s.tExt : s.extensionCommitment;
  const bst = custodyState('kcc20', BigInt(s.bought), rec.covenantId, ext);
  chain.add({ transactionId: TX, index: 1, amount: KAS.toString(), scriptPublicKey: kob.tokenScriptPublicKey(programOf(tpl), bst), covenantId: token, blockDaaScore: '1500' });
  if (opts2.custody) {
    const c = opts2.custody;
    chain.add({ transactionId: TX, index: 2, amount: KAS.toString(), scriptPublicKey: kob.tokenScriptPublicKey(programOf(c.tpl), custodyState('kcc20', c.amount, rec.covenantId, c.ext)), covenantId: c.token, blockDaaScore: '1500' });
  }
  return chain;
}

function expectCancels(plan: CancelPlan): ReturnType<typeof decodeSigning> {
  expect(plan.issues.filter((i) => i.severity === 'error')).toEqual([]);
  signAndValidate(kob, plan.built!);
  const s = decodeSigning({ kob, built: plan.built!, maker: MAKER.pk, expected: plan.expected, nodeInputs: nodeFactsOf(plan.built!) });
  expect(s.blocking.map((b) => `${b.code}: ${b.message}`)).toEqual([]);
  return s;
}
const spendsTx1 = (plan: CancelPlan): boolean => plan.built!.tx.inputs.some((i) => i.transactionId === TX && i.index === 1);

describe('a Collected order after a partial fill, from node data only (R3A-1)', () => {
  it('KobBid: bought moves by the fill; the order and its bought-custody are found and the cancel returns both', async () => {
    const { rec, st } = collectedRecord('create.bid', { bought: '0' });
    const minFill = BigInt(F(st).minFill);
    const n = 2n * minFill;
    const cont = { ...st, state: { ...st.state, bought: n.toString() } } as OrderState;
    const chain = chainWith(rec, cont);
    const r = await resolveRecord(kob, chain, rec, testAddress);
    expect(r.status).toBe('live');
    expect(F(r.order!.state).bought).toBe(n.toString());
    expect(r.boughtCustody).toMatchObject({ transactionId: TX, index: 1, state: { amount: n.toString(), owner: rec.covenantId } });
    expect(r.boughtIssue).toBeNull();
    const plan = planCancel(env(), snapshotFromRecord(r));
    expectCancels(plan);
    expect(spendsTx1(plan)).toBe(true);
    expect(plan.tokensReturned).toBe(n);
    // negative: the custody missing on the node is reported as looked for and not found
    const bare = new FakeChain();
    bare.add({ transactionId: TX, index: 0, amount: (20n * KAS).toString(), scriptPublicKey: kob.scriptPublicKey(cont), covenantId: rec.covenantId });
    const miss = await resolveRecord(kob, bare, rec, testAddress);
    expect(miss.status).toBe('live');
    expect(miss.boughtCustody).toBeNull();
    expect(miss.boughtIssue).toBe('not-found');
  });

  it('KobCondBid (stop-buy): amountLeft and bought move together', async () => {
    const { rec, st } = collectedRecord('create.condBid', { bought: '0' });
    const minFill = BigInt(F(st).minFill);
    const left = BigInt(F(st).amountLeft) - minFill;
    const cont = { ...st, state: { ...st.state, amountLeft: left.toString(), bought: minFill.toString() } } as OrderState;
    const r = await resolveRecord(kob, chainWith(rec, cont), rec, testAddress);
    expect(r.status).toBe('live');
    expect(F(r.order!.state)).toMatchObject({ amountLeft: left.toString(), bought: minFill.toString() });
    expect(r.boughtCustody?.state.amount).toBe(minFill.toString());
    const plan = planCancel(env(), snapshotFromRecord(r));
    expectCancels(plan);
    expect(plan.tokensReturned).toBe(minFill);
  });

  it('pair sell at 1.5 B per A: 100 A placed, 40 sold, bought = tOut = 60; custody of A and bought B are both returned', async () => {
    const p0 = collectedRecord('pair.create.ask', {});
    const sA = BigInt(F(p0.st).sScale);
    const price = (3n * sA) / 2n;
    const { rec, st } = collectedRecord('pair.create.ask', {
      bought: '0', amountLeft: '100', custody: '100', minFill: '1', price: price.toString(), priceEnd: price.toString(), slope: '0', maxFill: '0', interval: '0',
    });
    const cont = { ...st, state: { ...st.state, amountLeft: '60', custody: '60', bought: '60' } } as OrderState;
    const s = F(st);
    const chain = chainWith(rec, cont, { custody: { tpl: s.sTplHash, token: s.sCovId, ext: s.sExt, amount: 60n } });
    // The record keeps the placed custody's extension commitment.
    const r = await resolveRecord(kob, chain, rec, testAddress);
    expect(r.status).toBe('live');
    expect(F(r.order!.state)).toMatchObject({ amountLeft: '60', custody: '60', bought: '60' });
    expect(r.custody?.state.amount).toBe('60');
    expect(r.boughtCustody?.state.amount).toBe('60');
    const plan = planCancel(env(), snapshotFromRecord(r));
    const d = expectCancels(plan);
    expect(spendsTx1(plan)).toBe(true);
    expect(d.net.tokens.find((t) => t.ref.covenantId === s.sCovId)!.toMaker).toBe(60n);
    expect(d.net.tokens.find((t) => t.ref.covenantId === s.tCovId)!.toMaker).toBe(60n);
  });

  for (const name of ['pair.create.bid', 'pair.create.condBid'] as const) {
    it(`${name} (pair buy) at 1.5 B per A: 100 A wanted, 40 bought; the escrow pays floor(40 x 1.5) = 60 B, bought + 40; escrow and bought both return`, async () => {
      const p0 = collectedRecord(name, {});
      const aS = BigInt(F(p0.st).tScale);
      const price = ((3n * aS) / 2n).toString();
      const leg = name === 'pair.create.bid' ? { price, priceEnd: price, slope: '0', maxFill: '0', interval: '0' } : { tpPrice: price, stopPrice: '0' };
      const { rec, st } = collectedRecord(name, { bought: '0', amountLeft: '100', custody: '150', minFill: '1', ...leg });
      const cont = { ...st, state: { ...st.state, amountLeft: '60', custody: '90', bought: '40' } } as OrderState;
      const s = F(st);
      const chain = chainWith(rec, cont, { custody: { tpl: s.sTplHash, token: s.sCovId, ext: s.sExt, amount: 90n } });
      const r = await resolveRecord(kob, chain, rec, testAddress);
      expect(r.status).toBe('live');
      expect(F(r.order!.state)).toMatchObject({ amountLeft: '60', custody: '90', bought: '40' });
      expect(r.custody?.state.amount).toBe('90');
      expect(r.boughtCustody?.state.amount).toBe('40');
      const plan = planCancel(env(), snapshotFromRecord(r));
      const d = expectCancels(plan);
      expect(d.net.tokens.find((t) => t.ref.covenantId === s.sCovId)!.toMaker).toBe(90n);
      expect(d.net.tokens.find((t) => t.ref.covenantId === s.tCovId)!.toMaker).toBe(40n);
    });
  }

  it('negative: a hint for a state outside the candidate set is not accepted (bought beyond the rounding the walk tries)', async () => {
    const p0 = collectedRecord('pair.create.ask', {});
    const price = (3n * BigInt(F(p0.st).sScale)) / 2n;
    const { rec, st } = collectedRecord('pair.create.ask', {
      bought: '0', amountLeft: '100', custody: '100', minFill: '1', price: price.toString(), priceEnd: price.toString(), slope: '0', maxFill: '0', interval: '0',
    });
    const at = (bought: string) => {
      const spk = kob.scriptPublicKey({ ...st, state: { ...st.state, amountLeft: '60', custody: '60', bought } } as OrderState);
      return { address: testAddress(spk), transactionId: TX, index: 0, amount: KAS.toString(), scriptPublicKey: spk, blockDaaScore: '1', isCoinbase: false, covenantId: rec.covenantId };
    };
    expect((await resolveRecord(kob, new FakeChain(), rec, testAddress, { hints: [at('60')] })).status).toBe('live');
    expect((await resolveRecord(kob, new FakeChain(), rec, testAddress, { hints: [at('69')] })).status).toBe('unknown');
    expect((await resolveRecord(kob, new FakeChain(), rec, testAddress, { hints: [at('20')] })).status).toBe('unknown');
  });
});
