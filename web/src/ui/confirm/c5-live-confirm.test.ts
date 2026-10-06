// C5 (lifecycle disclosure): the pre-sign screen's "you receive at least / pay at most per token (all-in)" row is a GUARANTEE. For an auction
// (market, marketable limit, Dutch) it must be the auction's END (worst) price, not its start (the touch): confirm-model.ts `orderRows`
// computes `allIn` from `d.price`, the start, while the covenant only guarantees `priceEnd` (KobAsk.sil: p = max(priceEnd, price - slope·…)).
// Uncompiled when written (2026-10-02).
// C5: expected to fail until orderRows derives the all-in guarantee from `auction.priceEnd` when the order decays (and labels it "worst").
import { describe, expect, it } from 'vitest';
import { decodeSigning, describeInputsForWallet } from '../../kob/decode';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { ActionRequest } from '../../kob/types';
import { MAKER, goldenRequest, nodeFactsOf, tradableRegistryJson } from '../../testing/chain-fixtures';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { buildConfirmModel, type Row } from './confirm-model';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const CLOCK = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

const card = (name: string) => {
  const built = kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
  const summary = decodeSigning({ kob, built, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(built) });
  const m = buildConfirmModel(summary, { registry, clock: CLOCK, tr: (k, p) => t(k, p) }, describeInputsForWallet(built, 'kasware').notices);
  return m.sections.find((s) => s.id === 'create')!.cards[0]!;
};
const num = (rows: Row[], id: string): number => {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new Error(`no row ${id}: ${rows.map((x) => x.id).join(',')}`);
  return parseFloat(r.value.replace(/,/g, ''));
};

describe('C5-U1: the all-in guarantee of an auction is its worst price', () => {
  it('market SELL: "receive at least" is not above the auction end price', () => {
    const c = card('create.ask.market');
    expect(num(c.rows, 'allIn')).toBeLessThanOrEqual(num(c.rows, 'auction'));
  });

  it('market BUY: "pay at most" is not below the auction end price', () => {
    const c = card('create.bid.market');
    expect(num(c.rows, 'allIn')).toBeGreaterThanOrEqual(num(c.rows, 'auction'));
  });

  it('Dutch sell: same rule (the floor is priceEnd)', () => {
    const c = card('create.ask.dutch');
    if (c.rows.some((r) => r.id === 'allIn')) expect(num(c.rows, 'allIn')).toBeLessThanOrEqual(num(c.rows, 'auction'));
  });
});
