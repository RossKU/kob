import { describe, expect, it } from 'vitest';
import type { EventView } from '../../data/indexer-types';
import { groupPositions } from '../../kob/positions';
import { loadKobNode } from '../../kob/wasm.node';
import { orderViewOf, placeGolden } from '../../testing/chain-fixtures';
import { buildEntries, describeEntry } from './orders-model';
import { positionPhase, ratio, realisedPnl, summarizePosition, summarizePositionFills, unwindAmount, POSITION_PHASES } from './position-model';

const kob = loadKobNode();
const clock = { daa: 1000n, unixSeconds: 1_790_000_000n, rateMilli: 10_000 };
const id = (c: string) => c.repeat(32);
const genesis = (daa: number) => ({ txid: id('01'), out: 0, block_seq: daa, daa, confirmations: 10, settled: true });
const snap = (name: string) => placeGolden(kob, name).snapshots[0];
const entrySnap = snap('create.ifdBid');
const repeatSnap = snap('create.ifdBid.repeat');
const exitSnap = snap('create.condAsk');

type V = ReturnType<typeof orderViewOf>;
const entry = (over = {}): V => orderViewOf(kob, { ...entrySnap, covenantId: id('e1') }, { genesis: genesis(100), initial_amount: '10', amount_left: '10', filled_amount: '0', ...over });
const rentry = (over = {}): V =>
  orderViewOf(kob, { ...repeatSnap, covenantId: id('e2') }, { genesis: genesis(100), initial_amount: '4', amount_left: '4', filled_amount: '0', repeat: { role: 'entry', rpt_amount: '9', rearm_amount: '8' }, ...over });
const exit = (n: string, parent: string, over = {}): V =>
  orderViewOf(kob, { ...exitSnap, covenantId: id(n) }, { parent: id(parent), genesis: genesis(200 + parseInt(n, 16)), initial_amount: '3', amount_left: '3', ...over });

const rowsOf = (views: V[]) => new Map(buildEntries(views, []).map((e) => [e.id, describeEntry(e, { kob, clock })]));
const pos = (views: V[]) => groupPositions(views)[0]!;

describe('unwindAmount (close)', () => {
  it('buy first: base units bought by the entry and not yet sold by the exits', () => {
    expect(unwindAmount(pos([entry()]))).toBe(0n);
    expect(unwindAmount(pos([entry({ status: 'partial', filled_amount: '6', amount_left: '4' }), exit('a1', 'e1', { filled_amount: '2', amount_left: '1' }), exit('a2', 'e1')]))).toBe(4n);
  });

  it('sell first: base units sold by the entry and not yet bought back (never negative)', () => {
    const sellEntry = (over = {}): V => orderViewOf(kob, { ...snap('create.ifdAsk'), covenantId: id('e3') }, { genesis: genesis(100), initial_amount: '10', amount_left: '10', filled_amount: '0', ...over });
    const bidExit = (n: string, over = {}): V => orderViewOf(kob, { ...snap('create.condBid'), covenantId: id(n) }, { parent: id('e3'), genesis: genesis(300), initial_amount: '3', amount_left: '3', ...over });
    const p = pos([sellEntry({ status: 'partial', filled_amount: '5', amount_left: '5' }), bidExit('b1', { status: 'filled', filled_amount: '3', amount_left: '0' }), bidExit('b2', { filled_amount: '0', amount_left: '2' })]);
    expect(p.side).toBe('sell');
    expect(unwindAmount(p)).toBe(2n);
    expect(unwindAmount(pos([sellEntry({ status: 'filled', filled_amount: '3', amount_left: '0' }), bidExit('b1', { status: 'filled', filled_amount: '3', amount_left: '0' })]))).toBe(0n);
  });
});

describe('positionPhase', () => {
  it('entry waiting / partly filled / exit resting / repeat waiting', () => {
    expect(positionPhase(pos([entry()]))).toBe('entry-waiting');
    expect(positionPhase(pos([entry({ status: 'partial', filled_amount: '3', amount_left: '7' }), exit('a1', 'e1')]))).toBe('entry-partial');
    expect(positionPhase(pos([entry({ status: 'filled', filled_amount: '10', amount_left: '0' }), exit('a1', 'e1')]))).toBe('exit-resting');
    expect(positionPhase(pos([rentry({ status: 'partial', filled_amount: '4', amount_left: '0' }), exit('d1', 'e2', { repeat: { role: 'exit', parent: id('e2') } })]))).toBe('repeat-waiting');
  });

  it('ended without any trade is cancelled; ended after trading is closed', () => {
    expect(positionPhase(pos([entry({ status: 'cancelled', amount_left: '0' })]))).toBe('cancelled');
    expect(positionPhase(pos([entry({ status: 'cancelled', amount_left: '0' }), exit('a1', 'e1', { status: 'cancelled', amount_left: '0' })]))).toBe('cancelled');
    expect(positionPhase(pos([entry({ status: 'cancelled', filled_amount: '3', amount_left: '0' }), exit('a1', 'e1', { status: 'filled', filled_amount: '3', amount_left: '0' })]))).toBe('closed');
    expect(positionPhase(pos([entry({ status: 'filled', filled_amount: '10', amount_left: '0' })]))).toBe('closed');
  });

  it('the phase list is the dictionary family', () => {
    expect(POSITION_PHASES).toEqual(['entry-waiting', 'entry-partial', 'exit-resting', 'repeat-waiting', 'closed', 'cancelled']);
  });
});

describe('summarizePosition', () => {
  it('prices along entry -> exits, amounts entered / exited / open', () => {
    const views = [
      entry({ status: 'partial', filled_amount: '6', amount_left: '4' }),
      exit('a1', 'e1', { status: 'filled', filled_amount: '3', amount_left: '0' }),
      exit('a2', 'e1', { status: 'open', amount_left: '3' }),
    ];
    const rows = rowsOf(views);
    const s = summarizePosition(pos(views), rows, clock);
    expect(s.phase).toBe('entry-partial');
    expect(s.side).toBe('buy');
    expect(s.entryPrice).toBe(rows.get(id('e1'))!.price);
    expect(s.entryPrice).not.toBeNull();
    expect(s.token).toBe(rows.get(id('e1'))!.token);
    expect(s.amountTotal).toBe(10n);
    expect(s.amountEntered).toBe(6n);
    expect(s.amountExited).toBe(3n);
    expect(s.amountOpen).toBe(4n + 3n);
    // only the live exit contributes to the resting take-profit list
    expect(s.exits.map((e) => e.id)).toEqual([id('a1'), id('a2')]);
    expect(s.exitTakeProfits).toEqual([rows.get(id('a2'))!.price]);
    expect(s.repeat).toBeNull();
  });

  it('repeat: cycles, re-arms left and the latest rpt_until (DAA and wall clock)', () => {
    const views = [
      rentry({ status: 'partial', filled_amount: '4', amount_left: '0' }),
      exit('d1', 'e2', { status: 'filled', amount_left: '0', repeat: { role: 'exit', parent: id('e2'), rpt_until: '5000' } }),
      exit('d2', 'e2', { repeat: { role: 'exit', parent: id('e2'), rpt_until: '7000' } }),
    ];
    const s = summarizePosition(pos(views), rowsOf(views), clock);
    expect(s.phase).toBe('repeat-waiting');
    expect(s.repeat).toMatchObject({ cyclesDone: 1, rearmAmount: 8n, untilDaa: 7000n });
    expect(s.repeat!.untilUnix).toBeGreaterThan(clock.unixSeconds);
    expect(summarizePosition(pos(views), rowsOf(views), null).repeat!.untilUnix).toBeNull();
  });

  it('the KRON contracts group and phase exactly like the KCC-20 ones', () => {
    const views = [entry({ contract: 'KobIfdBidKron', status: 'partial', filled_amount: '3', amount_left: '7' }), exit('a1', 'e1', { contract: 'KobCondAskKron' })];
    const p = pos(views);
    expect(p.kind).toBe('ifd');
    expect(positionPhase(p)).toBe('entry-partial');
    expect(groupPositions([entry({ contract: 'KobIfdAskKron' })])[0]!.kind).toBe('ifd');
  });
});

describe('summarizePositionFills', () => {
  const ev = (idn: number, amount: number, price: string | null, payout: string | null = null, kind = 'fill'): EventView => ({
    id: idn, covenant_id: id('e1'), block_seq: idn, daa: idn, ts: idn, txid: id('aa'), tx_pos: 0, kind, token: null, side: 2, amount: String(amount), price, payout, closes: false, detail: null,
    confirmations: 5, settled: true,
  });

  it('sums base units, averages the price per whole token weighted by amount, counts an event once, ignores non-fills', () => {
    const e = [ev(1, 200, '24000'), ev(2, 400, '26000'), ev(2, 400, '26000'), ev(3, 0, '1', null, 'cancel')];
    const x = [ev(10, 300, '30000', '900')];
    const f = summarizePositionFills(e, x, 'buy', 100);
    expect(f.entry).toEqual({ fills: 2, amount: 600n, avgPrice: (24000n * 200n + 26000n * 400n) / 600n, payout: 0n });
    expect(f.exits).toMatchObject({ fills: 1, amount: 300n, avgPrice: 30000n, payout: 900n });
    expect(f.spreadPerToken).toBe(30000n - f.entry.avgPrice!);
    expect(f.scale).toBe(100n);
    // sell first: the entry sold high, the exit buys back low
    expect(summarizePositionFills(x, e, 'sell', 100).spreadPerToken).toBe(30000n - f.entry.avgPrice!);
  });

  it('no fills: nothing to average, spread unknown', () => {
    const f = summarizePositionFills([], [], 'buy');
    expect(f).toEqual({ entry: { fills: 0, amount: 0n, avgPrice: null, payout: 0n }, exits: { fills: 0, amount: 0n, avgPrice: null, payout: 0n }, spreadPerToken: null, scale: 1n });
  });
});

describe('realisedPnl', () => {
  const leg = (amount: bigint, avgPrice: bigint | null) => ({ fills: amount ? 1 : 0, amount, avgPrice, payout: 0n });
  const fill = (n: number, cov: string, amount: number, price: string): EventView => ({
    id: n, covenant_id: id(cov), block_seq: n, daa: n, ts: n, txid: id('aa'), tx_pos: 0, kind: 'fill', token: null, side: 2, amount: String(amount), price, payout: null, closes: false, detail: null,
    confirmations: 5, settled: true,
  });

  it('spread per whole token times the amount both entered and exited over the scale, with the return on the entry price', () => {
    expect(realisedPnl({ entry: leg(6n, 25_000n), exits: leg(3n, 27_000n), spreadPerToken: 2_000n, scale: 1n })).toEqual({ amountClosed: 3n, pnl: 6_000n, returnPct: 8 });
    // scale 1e8: 1.5 tokens closed at a spread of 2,000 sompi per token = 3,000 sompi
    expect(realisedPnl({ entry: leg(150_000_000n, 25_000n), exits: leg(200_000_000n, 27_000n), spreadPerToken: 2_000n, scale: 100_000_000n })).toMatchObject({ amountClosed: 150_000_000n, pnl: 3_000n });
    // a part of a sompi rounds toward zero, either sign
    expect(realisedPnl({ entry: leg(1n, 25_000n), exits: leg(1n, 27_000n), spreadPerToken: 2_000n, scale: 100_000_000n }).pnl).toBe(0n);
    expect(realisedPnl({ entry: leg(1n, 27_000n), exits: leg(1n, 25_000n), spreadPerToken: -2_000n, scale: 100_000_000n }).pnl).toBe(0n);
  });

  it('a losing round trip is negative', () => {
    expect(realisedPnl({ entry: leg(2n, 30_000n), exits: leg(2n, 27_000n), spreadPerToken: -3_000n, scale: 1n })).toEqual({ amountClosed: 2n, pnl: -6_000n, returnPct: -10 });
  });

  it('nothing closed yet realises zero; unpriced fills are unknown, never NaN', () => {
    expect(realisedPnl({ entry: leg(4n, 25_000n), exits: leg(0n, null), spreadPerToken: null, scale: 1n })).toEqual({ amountClosed: 0n, pnl: 0n, returnPct: null });
    expect(realisedPnl({ entry: leg(2n, null), exits: leg(2n, null), spreadPerToken: null, scale: 1n })).toEqual({ amountClosed: 2n, pnl: null, returnPct: null });
    expect(realisedPnl({ entry: leg(2n, 0n), exits: leg(2n, 5n), spreadPerToken: 5n, scale: 1n }).returnPct).toBeNull();
  });

  it('works on summarizePositionFills output (sell first: sold at 30,000, bought back at 25,000 sompi per 100 units, 200 units)', () => {
    const f = summarizePositionFills([fill(1, 'e1', 200, '30000')], [fill(2, 'a1', 200, '25000')], 'sell', 100);
    expect(realisedPnl(f)).toEqual({ amountClosed: 200n, pnl: 10_000n, returnPct: 16.66 });
  });
});

describe('ratio', () => {
  it('clamps to 0..1 and is null for an unknown or empty denominator', () => {
    expect(ratio(1n, 4n)).toBe(0.25);
    expect(ratio(5n, 4n)).toBe(1);
    expect(ratio(-1n, 4n)).toBe(0);
    expect(ratio(0n, 0n)).toBeNull();
    expect(ratio(1n, null)).toBeNull();
    expect(ratio(null, 3n)).toBeNull();
    // exact beyond 2^53 base units
    expect(ratio(12_345_678_901_234_567n, 24_691_357_802_469_134n)).toBe(0.5);
  });
});
