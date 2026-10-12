// Pre-mainnet audit (A1): what the pre-sign decoder shows against what is signed, for transactions that did NOT come from the app's own builders
// (a compromised planner path, a hostile kob-wasm, a hand-made transaction) and for indexer-chosen token facts.
import { describe, expect, it } from 'vitest';
import { decodeSigning, type SigningSummary } from './decode';
import { parseRegistry, withExtraToken, type TokenInfo, type TokenRegistry } from './registry';
import { loadKobNode } from './wasm.node';
import type { BuiltTx, Hex, OrderState, PayloadRecord } from './types';
import type { KobWasm } from './wasm';
import { MAKER, OTHER, placeGolden, tradableRegistryJson, nodeFactsOf } from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const decode = (built: BuiltTx, reg: TokenRegistry | null = registry, k: KobWasm = kob): SigningSummary =>
  decodeSigning({ kob: k, built, maker: MAKER.pk, registry: reg, nodeInputs: nodeFactsOf(built) });
const codes = (xs: { code: string }[]) => xs.map((x) => x.code);

/**
 * A placement transaction whose order (output `out`) is `next` instead: the output script is the P2SH of `next` and the placement record
 * carries `next`, as a hand-made (or compromised-builder) transaction would. The genesis covenant id of such a transaction is a hash of its
 * outputs that kob-wasm does not export, so `recoverOrders` is wrapped to return the record as kob-protocol would recover it from that
 * transaction (`next` at the same covenant id); every other check of the decoder runs on the real kob-wasm.
 */
function withOrder(built: BuiltTx, next: OrderState, out = 0): { built: BuiltTx; kob: KobWasm } {
  const b = clone(built);
  const p = kob.decodePayload(b.tx.payload)!;
  const records = p.records.map((r: PayloadRecord) => (r.type === 'order' && r.output === out ? { ...r, state: kob.encodeState(next) } : r));
  b.tx.payload = kob.encodePayload(records);
  b.tx.outputs[out]!.scriptPublicKey = kob.scriptPublicKey(next);
  const recovered = kob.recoverOrders(built.tx).map((r) => (r.output === out ? { ...r, order: kob.decodeState(next.kind, kob.encodeState(next)) } : r));
  return { built: b, kob: { ...kob, recoverOrders: () => clone(recovered) } };
}

describe('A1: an if-done entry whose committed exit pays another key', () => {
  for (const name of ['create.ifdBid', 'create.ifdAsk']) {
    it(`${name}: the entry names the wallet as maker, its exit names OTHER: blocking (order-terms-invalid)`, () => {
      const placed = placeGolden(kob, name);
      const entry = placed.recovered[0]!.order as OrderState;
      const st = entry.state as unknown as { exitState: string; maker: Hex };
      expect(st.maker).toBe(MAKER.pk);
      // the maker key appears once in the committed exit span: swap it for another key
      expect(st.exitState.split(MAKER.pk)).toHaveLength(2);
      const hostile = { ...entry, state: { ...entry.state, exitState: st.exitState.replace(MAKER.pk, OTHER.pk) } } as OrderState;
      const forged = withOrder(placed.built, hostile);
      // the output is the P2SH of the hostile state and its record carries it: only the new-order rules can catch it
      const s = decode(forged.built, registry, forged.kob);
      expect(codes(s.blocking)).toContain('order-terms-invalid');
      expect(s.ok).toBe(false);
      // without the re-check the entry itself looks like the wallet's order (its maker is the wallet): nothing else flags it
      expect(codes(s.blocking).filter((c) => c !== 'order-terms-invalid')).toEqual([]);
      // the honest placement is clean
      expect(decode(placed.built).blocking).toEqual([]);
    });
  }

  it('a hostile kob-wasm whose new-order rules refuse is surfaced for a KAS order too', () => {
    const placed = placeGolden(kob, 'create.condAsk');
    const s = decodeSigning({ kob: { ...kob, checkNewOrder: () => { throw new Error('slipBps must be within 0..=10000'); } }, built: placed.built, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(placed.built) });
    expect(codes(s.blocking)).toEqual(['order-terms-invalid']);
    expect(s.blocking[0]!.message).toContain('slipBps');
  });
});

describe('A1: an open-list KCC-20 token: the extension commitment is the indexer\'s and is not shown', () => {
  it('a bid that pins ANY extension commitment decodes without a finding once the indexer\'s token is added to the registry', () => {
    const placed = placeGolden(kob, 'create.bid');
    const bid = placed.recovered[0]!.order as OrderState;
    const cov = (bid.state as unknown as { tokenCovId: Hex }).tokenCovId;
    const lookalike = 'ab'.repeat(32);
    const hostile = { ...bid, state: { ...bid.state, extensionCommitment: lookalike } } as OrderState;
    const forged = withOrder(placed.built, hostile);
    // against the shipped registry entry (another commitment) it is blocking
    expect(codes(decode(forged.built, registry, forged.kob).blocking)).toEqual(['order-token-mismatch']);
    // an open-list token synthesised from an indexer that reports the look-alike commitment (open-token.ts: `view.extension_commitment`)
    const base = registry.byCovenantId.get(cov)!;
    const open: TokenInfo = { ...base, extensionCommitment: lookalike, openList: true, json: { ...base.json, extension_commitment: lookalike } };
    const reg = withExtraToken({ ...registry, tokens: registry.tokens.filter((t) => t.covenantId !== cov), byCovenantId: new Map([...registry.byCovenantId].filter(([k]) => k !== cov)) }, open);
    const s = decode(forged.built, reg, forged.kob);
    expect(s.blocking).toEqual([]);
    expect(codes(s.warnings)).not.toContain('order-token-mismatch');
    // nothing in the decoded order names the commitment the bid will receive: the screen identifies the token by covenant id only
    expect(JSON.stringify(s.orders[0]!.description, (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).not.toContain(lookalike);
  });
});
