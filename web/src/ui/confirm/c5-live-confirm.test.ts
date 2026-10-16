// C5 (lifecycle disclosure): the "you receive at least / pay at most per token (all-in)" row of `orderRows` is a GUARANTEE. For an auction
// (market, marketable limit, Dutch) it must be the auction's END (worst) price, not its start (the touch): the covenant only guarantees
// `priceEnd` (KobAsk.sil: p = max(priceEnd, price - slope·…)), so confirm-model.ts derives `allIn` from `auction.priceEnd` when the order decays.
import { describe, expect, it } from 'vitest';
import { decodeSigning } from '../../kob/decode';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { ActionRequest } from '../../kob/types';
import { MAKER, goldenRequest, nodeFactsOf, tradableRegistryJson } from '../../testing/chain-fixtures';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { orderRows, type Row } from './confirm-model';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const CLOCK = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

/** The rows of the golden transaction's created order. */
const orderRowsOf = (name: string): Row[] => {
  const built = kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
  const summary = decodeSigning({ kob, built, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(built) });
  const o = summary.orders[0]!;
  const info = registry.byCovenantId.get(o.description.tokenCovId)!;
  const ref = { covenantId: info.covenantId, ticker: info.ticker, decimals: info.decimals, display: '', inRegistry: true, tradable: info.tradable };
  return orderRows(o.description, { value: o.value, locked: o.locked, deadline: o.deadline }, { tr: (k, p) => t(k, p), ref, clock: CLOCK, time: (unix) => `@${unix}`, registry });
};
const num = (rows: Row[], id: string): number => {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new Error(`no row ${id}: ${rows.map((x) => x.id).join(',')}`);
  return parseFloat(r.value.replace(/,/g, ''));
};

describe('C5-U1: the all-in guarantee of an auction is its worst price', () => {
  it('market SELL: "receive at least" is not above the auction end price', () => {
    const rows = orderRowsOf('create.ask.market');
    expect(num(rows, 'allIn')).toBeLessThanOrEqual(num(rows, 'auction'));
  });

  it('market BUY: "pay at most" is not below the auction end price', () => {
    const rows = orderRowsOf('create.bid.market');
    expect(num(rows, 'allIn')).toBeGreaterThanOrEqual(num(rows, 'auction'));
  });

  it('Dutch sell: same rule (the floor is priceEnd)', () => {
    const rows = orderRowsOf('create.ask.dutch');
    if (rows.some((r) => r.id === 'allIn')) expect(num(rows, 'allIn')).toBeLessThanOrEqual(num(rows, 'auction'));
  });
});
