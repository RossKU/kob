// Pre-mainnet audit (A1): what the pre-sign decoder shows against what is signed, for transactions that did NOT come from the app's own builders
// (a compromised planner path, a hostile kob-wasm, a hand-made transaction) and for indexer-chosen token facts.
import { describe, expect, it } from 'vitest';
import { decodeSigning, type SigningSummary } from './decode';
import { parseRegistry, withExtraToken, type ExtensionReference, type TokenInfo, type TokenRegistry } from './registry';
import { openTokenIdentity } from '../ui/confirm/confirm-model';
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

describe('A1-2: an open-list KCC-20 token: the commitment is the indexer\'s pick; the identity rows show it and a known mismatch blocks', () => {
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
    // nothing in the decoded order names the commitment the bid will receive: the order identifies the token by covenant id only
    expect(JSON.stringify(s.orders[0]!.description, (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).not.toContain(lookalike);

    // the identity rows show the commitment and its source (the fix of A1-2): the user can check it
    const withExt = (refs: ExtensionReference[]): TokenRegistry => {
      const info: TokenInfo = { ...open, extension: { commitment: lookalike, source: 'indexer', references: refs, mismatch: refs.filter((r) => r.commitment !== lookalike) } };
      return withExtraToken({ ...registry, tokens: registry.tokens.filter((t) => t.covenantId !== cov), byCovenantId: new Map([...registry.byCovenantId].filter(([k]) => k !== cov)) }, info);
    };
    const shown = (r: TokenRegistry) => {
      const summary = decode(forged.built, r, forged.kob);
      const identity = openTokenIdentity(summary, r);
      return { ...identity, canSign: summary.ok && identity.blocking.length === 0 };
    };
    const plain = shown(withExt([]));
    const idRow = plain.rows[0]!;
    expect(idRow.value).toBe(lookalike);
    expect(idRow.tone).toBe('warn');
    expect(idRow.detail).toContain("indexer's token list");
    expect(plain.canSign).toBe(true);
    // a commitment known without the indexer (node-verified holdings, a genesis-verified registry entry) that differs blocks signing
    const genuine = base.extensionCommitment!;
    for (const source of ['holdings', 'registry-genesis'] as const) {
      const m = shown(withExt([{ commitment: genuine, source }]));
      expect(m.blocking.map((b) => b.code)).toEqual(['open-token-extension-mismatch']);
      expect(m.blocking[0]!.text).toContain(genuine);
      expect(m.canSign).toBe(false);
      expect(m.rows[0]!.tone).toBe('bad');
    }
    // a matching reference is shown as confirmed
    const good = shown(withExt([{ commitment: lookalike, source: 'holdings' }]));
    expect(good.blocking).toEqual([]);
    expect(good.rows[0]!.tone).toBe('good');
    // a registry token is not an open-list token: no identity rows
    expect(openTokenIdentity(decode(placed.built), registry).rows).toEqual([]);
  });

  it('a first order (decimals entered, no order of the token yet) shows its decimals; a commitment read from the holdings says so', () => {
    const placed = placeGolden(kob, 'create.bid');
    const bid = placed.recovered[0]!.order as OrderState;
    const cov = (bid.state as unknown as { tokenCovId: Hex }).tokenCovId;
    const base = registry.byCovenantId.get(cov)!;
    const ext = base.extensionCommitment!;
    const info: TokenInfo = {
      ...base, openList: true, openScale: 'entered',
      extension: { commitment: ext, source: 'holdings', references: [{ commitment: ext, source: 'holdings' }], mismatch: [] },
    };
    const reg = withExtraToken({ ...registry, tokens: registry.tokens.filter((t) => t.covenantId !== cov), byCovenantId: new Map([...registry.byCovenantId].filter(([k]) => k !== cov)) }, info);
    const summary = decode(placed.built, reg);
    const { rows, blocking } = openTokenIdentity(summary, reg);
    expect(rows.map((r) => r.id)).toEqual([`scale-${cov}`, `ext-${cov}`]);
    expect(rows[0]!.value).toBe(`${base.decimals} (entered by you)`);
    expect(rows[0]!.tone).toBe('warn');
    expect(rows[0]!.detail).toContain('declares no decimals on chain');
    expect(rows[1]!.detail).toContain('node-verified holdings');
    expect(rows[1]!.tone).toBe('good');
    expect(summary.ok && blocking.length === 0).toBe(true);
  });
});
