// The rows of a pair placement (the golden pair vectors, decoded from the built transaction only): they name the pair, what the order holds of A
// and B, what the whole amount guarantees in B, the KAS tip, and the founder's trigger rule of a pair stop; no raw key or hole is shown.
import { describe, expect, it } from 'vitest';
import { decodeSigning, type DecodedOrder, type TokenRef } from '../../kob/decode';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { ActionRequest, BuiltTx } from '../../kob/types';
import { MAKER, goldenRequest, nodeFactsOf, tradableRegistryJson } from '../../testing/chain-fixtures';
import { PAIR_CREATES } from '../../testing/pair-fixtures';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { orderRows, orderTitle, type Fmt, type Row } from './confirm-model';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const CLOCK = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

/** The created pair order, its rows and title, as the signer frame builds them. */
function pairOrder(name: string, reg: TokenRegistry | null = registry): { order: DecodedOrder; rows: Row[]; title: string } {
  const built: BuiltTx = kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
  const summary = decodeSigning({ kob, built, maker: MAKER.pk, registry: reg, nodeInputs: nodeFactsOf(built) });
  if (!summary.orders[0]) throw new Error(`no created order: ${summary.kind}`);
  const order = summary.orders[0];
  const info = reg?.byCovenantId.get(order.description.tokenCovId);
  const ref: TokenRef = info
    ? { covenantId: info.covenantId, ticker: info.ticker, decimals: info.decimals, display: '', inRegistry: true, tradable: info.tradable }
    : { covenantId: order.description.tokenCovId, ticker: null, decimals: null, display: '', inRegistry: false, tradable: false };
  const f: Fmt = { tr: (k, p) => t(k, p), ref, clock: CLOCK, time: (unix) => `@${unix}`, registry: reg };
  return { order, rows: orderRows(order.description, { value: order.value, locked: order.locked, deadline: order.deadline }, f), title: orderTitle(order.description, order.deadline, f) };
}
const ids = (rows: Row[]) => rows.map((r) => r.id);

describe('rows of a pair placement', () => {
  for (const [vec, kind, side] of PAIR_CREATES) {
    it(`pair.${vec}: ${kind} ${side}`, () => {
      for (const reg of [registry, null]) {
        const { order, rows, title } = pairOrder(`pair.${vec}`, reg);
        expect(order.verified).toBe(true);
        expect(ids(rows)).toContain('pairPair');
        expect(rows.find((r) => r.id === 'pairPair')!.value).toContain(side === 'sell' ? 'sells' : 'buys');
        // what it holds: an ask its A, a bid its B escrow, a sell-first entry both
        if (side === 'sell') expect(ids(rows)).toContain('pairEscrowA');
        if (side === 'buy' || kind === 'KobIfdPair') expect(ids(rows)).toContain('pairEscrowB');
        if (kind === 'KobPair') expect(ids(rows)).toContain('pairTotal');
        // KAS exposure rows of the KAS kinds never appear on a pair order: its trigger is the pair rule
        expect(ids(rows)).not.toContain('exposure');
        for (const s of [title, ...rows.flatMap((r) => [r.label, r.value, r.detail ?? ''])]) {
          expect(s, s).not.toMatch(/\{\w+\}/);
          expect(s, s).not.toMatch(/^(confirm|ticket|common)\.[\w.]+$/);
        }
      }
    });
  }

  it('a pair stop shows the founder\'s trigger rule (two KAS books or a resting pair order) with its stop in B per whole A', () => {
    const { rows } = pairOrder('pair.create.condAsk');
    const r = rows.find((x) => x.id === 'pairTrigger');
    expect(r, ids(rows).join(',')).toBeDefined();
    expect(r!.value).toMatch(/^arms on fills implying a rate at or below .+: a resting sell of .+ and buy of .+ filled together \(each rested [\d.]+ s\), or a resting .+ pair sell at or below the stop$/);
    const buy = pairOrder('pair.create.condBid').rows.find((x) => x.id === 'pairTrigger');
    expect(buy?.value).toContain('at or above');
  });
});
