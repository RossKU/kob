// The pre-sign screen of a pair placement (the golden pair vectors, decoded from the built transaction only): the card names the pair, what the order
// holds of A and B, what the whole amount guarantees in B, the KAS tip, and the founder's trigger rule of a pair stop; no raw key or hole is shown.
import { describe, expect, it } from 'vitest';
import { decodeSigning, describeInputsForWallet } from '../../kob/decode';
import { parseRegistry, type TokenRegistry } from '../../kob/registry';
import type { ActionRequest, BuiltTx } from '../../kob/types';
import { MAKER, goldenRequest, nodeFactsOf, tradableRegistryJson } from '../../testing/chain-fixtures';
import { PAIR_CREATES } from '../../testing/pair-fixtures';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { buildConfirmModel, type Card, type ConfirmModel, type Row } from './confirm-model';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const CLOCK = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

function model(name: string, reg: TokenRegistry | null = registry): ConfirmModel {
  const built: BuiltTx = kob.build(goldenRequest<ActionRequest>(name, MAKER.pk));
  const summary = decodeSigning({ kob, built, maker: MAKER.pk, registry: reg, nodeInputs: nodeFactsOf(built) });
  return buildConfirmModel(summary, { registry: reg, clock: CLOCK, tr: (k, p) => t(k, p) }, describeInputsForWallet(built, 'kasware').notices);
}
const cardOf = (m: ConfirmModel): Card => {
  const s = m.sections.find((x) => x.id === 'create');
  if (!s || !s.cards[0]) throw new Error(`no created order: ${m.sections.map((x) => x.id).join(',')}`);
  return s.cards[0];
};
const ids = (rows: Row[]) => rows.map((r) => r.id);
function texts(m: ConfirmModel): string[] {
  const out: string[] = [m.heading, m.intro];
  const walk = (c: Card) => {
    out.push(c.title, c.badge, ...c.rows.flatMap((r) => [r.label, r.value, r.detail ?? '']));
    c.children.forEach(walk);
  };
  for (const s of m.sections) {
    out.push(s.title, s.note ?? '', ...s.rows.flatMap((r) => [r.label, r.value, r.detail ?? '']));
    s.cards.forEach(walk);
  }
  return out;
}

describe('confirmation screen of a pair placement', () => {
  for (const [vec, kind, side] of PAIR_CREATES) {
    it(`pair.${vec}: ${kind} ${side}`, () => {
      for (const reg of [registry, null]) {
        const m = model(`pair.${vec}`, reg);
        const c = cardOf(m);
        expect(c.badge).toBe(t('confirm.verified'));
        expect(ids(c.rows)).toContain('pairPair');
        expect(c.rows.find((r) => r.id === 'pairPair')!.value).toContain(side === 'sell' ? 'sells' : 'buys');
        // what it holds: an ask its A, a bid its B escrow, a sell-first entry both
        if (side === 'sell') expect(ids(c.rows)).toContain('pairEscrowA');
        if (side === 'buy' || kind === 'KobIfdPair') expect(ids(c.rows)).toContain('pairEscrowB');
        if (kind === 'KobPair') expect(ids(c.rows)).toContain('pairTotal');
        // KAS exposure rows of the KAS kinds never appear on a pair order: its trigger is the pair rule
        expect(ids(c.rows)).not.toContain('exposure');
        for (const s of texts(m)) {
          expect(s, s).not.toMatch(/\{\w+\}/);
          expect(s, s).not.toMatch(/^(confirm|ticket|common)\.[\w.]+$/);
        }
      }
    });
  }

  it('a pair stop shows the founder\'s trigger rule (two KAS books or a resting pair order) with its stop in B per whole A', () => {
    const c = cardOf(model('pair.create.condAsk'));
    const r = c.rows.find((x) => x.id === 'pairTrigger');
    expect(r, ids(c.rows).join(',')).toBeDefined();
    expect(r!.value).toMatch(/^arms when the two KAS books imply a rate at or below .+ \(a resting sell of .+ and a resting buy of .+, each rested [\d.]+ s and filled together\), or when a resting pair order selling .+ at or below the stop is filled$/);
    const buy = cardOf(model('pair.create.condBid')).rows.find((x) => x.id === 'pairTrigger');
    expect(buy?.value).toContain('at or above');
  });
});
