// Test helpers of the ticket: one valid form per order type and side, priced around the default test book (asks from 2.50, bids from 2.45 KAS per token).
import { level, makeEnv, market8x8, type EnvOptions } from '../../testing/fixtures';
import type { BookView, PlanEnv } from '../../kob/plan-types';
import { initialForm, type OrderTypeId, type Side, type TicketCtx, type TicketForm } from './form-state';

// 8 decimals (scale 1e8: prices are sompi per whole token), tick 100 sompi per token (the mock registry's EXKCC)
export const MARKET = market8x8({ decimals: 8, tick: 100 });
export const CTX: TicketCtx = { decimals: 8, scale: 100_000_000n, tick: 100n };
// a token of 12 decimals: its orders quote per 10^9 base units (the scale cap), so KAS per token and the state price differ
/** One whole token of MARKET: 10^8 base units. */
export const TOKEN = 100_000_000n;

/** The default test book in MARKET's units: best ask 2.50, best bid 2.45 KAS per token, 5 / 10 / 20 tokens per level. */
export function ticketBook(): BookView {
  return {
    asks: [level(250_000_000n, 5n * TOKEN, 2), level(252_000_000n, 10n * TOKEN, 3), level(260_000_000n, 20n * TOKEN, 4)],
    bids: [level(245_000_000n, 5n * TOKEN, 1), level(243_000_000n, 10n * TOKEN, 2), level(240_000_000n, 20n * TOKEN, 3)],
  };
}

/** A PlanEnv for MARKET: 1,000 KAS, 100 tokens in one UTXO and the default book (each overridable). */
export const ticketEnv = (o: EnvOptions = {}): PlanEnv => makeEnv({ market: MARKET, tokenAmounts: [100n * TOKEN], book: ticketBook(), ...o });

export const WIDE: TicketCtx = { decimals: 12, scale: 1_000_000_000n, tick: 1n };

export const json = (x: unknown) => JSON.stringify(x, (_k, v) => (typeof v === 'bigint' ? `${v}n` : v));

export const form = (type: OrderTypeId, side: Side, values: Record<string, string>): TicketForm => {
  const f = initialForm(type, side);
  return { ...f, values: { ...f.values, ...values } };
};

/** One valid form per type and side, priced around the default test book (asks from 2.50, bids from 2.45 KAS per token); amounts in tokens. */
export const CASES: [string, OrderTypeId, Side, Record<string, string>][] = [
  ['limit sell', 'limit', 'sell', { amount: '5', price: '2.6' }],
  ['limit buy', 'limit', 'buy', { amount: '5', price: '2.3' }],
  ['limit day', 'limit', 'sell', { amount: '5', price: '2.6', lifetime: 'day' }],
  ['limit gtd', 'limit', 'buy', { amount: '5', price: '2.3', lifetime: 'gtd', lifetimeAt: '2026-10-05T00:00' }],
  ['ioc sell', 'ioc', 'sell', { amount: '3', price: '2.4' }],
  ['ioc buy', 'ioc', 'buy', { amount: '3', price: '2.55' }],
  ['fok sell', 'fok', 'sell', { amount: '3', price: '2.4' }],
  ['fok buy', 'fok', 'buy', { amount: '3', price: '2.55' }],
  ['market sell', 'market', 'sell', { amount: '2' }],
  ['market buy', 'market', 'buy', { amount: '2' }],
  ['streaming sell', 'streaming', 'sell', { amount: '2', displayedPrice: '2.45' }],
  ['streaming buy', 'streaming', 'buy', { amount: '2', displayedPrice: '2.5', allOrNothing: 'true' }],
  ['close all', 'close', 'sell', {}],
  ['close 4 tokens', 'close', 'sell', { amount: '4' }],
  ['twap', 'twap', 'sell', { amount: '10', sliceAmount: '2', interval: '5', price: '2.6' }],
  ['twap auction slices', 'twap', 'sell', { amount: '10', sliceAmount: '2', interval: '5', price: '2.6', priceEnd: '2.55' }],
  ['dca', 'dca', 'buy', { amount: '10', sliceAmount: '2', interval: '5', price: '2.3' }],
  ['dutch sell', 'dutch', 'sell', { amount: '5', price: '2.8', priceEnd: '2.6', duration: '10' }],
  ['dutch buy', 'dutch', 'buy', { amount: '5', price: '2.2', priceEnd: '2.4', duration: '10' }],
  ['stop-market sell', 'stopMarket', 'sell', { amount: '5', stop: '2.2' }],
  ['stop-market buy', 'stopMarket', 'buy', { amount: '5', stop: '2.8' }],
  ['stop-limit sell', 'stopLimit', 'sell', { amount: '5', stop: '2.2', limit: '2.1' }],
  ['stop-limit buy', 'stopLimit', 'buy', { amount: '5', stop: '2.8', limit: '2.9' }],
  ['trailing sell', 'trailingStop', 'sell', { amount: '5', stop: '2.2', 'trail.step': '0.05', 'trail.gap': '0.1' }],
  ['trailing sell + tp', 'trailingStop', 'sell', { amount: '5', stop: '2.2', 'trail.step': '0.05', 'trail.gap': '0.1', takeProfit: '2.9' }],
  ['take-profit sell', 'takeProfit', 'sell', { amount: '5', price: '2.7' }],
  ['take-profit buy', 'takeProfit', 'buy', { amount: '5', price: '2.2' }],
  ['oco sell', 'oco', 'sell', { amount: '5', takeProfit: '2.7', stop: '2.2' }],
  ['oco buy', 'oco', 'buy', { amount: '5', takeProfit: '2.2', stop: '2.8' }],
  ['ifd buy-first tp', 'ifd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6' }],
  ['ifd buy-first stop', 'ifd', 'buy', { amount: '4', price: '2.3', 'exit.kind': 'stop', 'exit.stop': '2.1' }],
  ['ifd sell-first', 'ifd', 'sell', { amount: '4', price: '2.7', 'exit.takeProfit': '2.4' }],
  ['ifd stop entry', 'ifd', 'buy', { amount: '4', price: '2.3', 'entry.stop': '2.2', 'exit.takeProfit': '2.6' }],
  ['ifo buy-first', 'ifo', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', 'exit.stop': '2.1' }],
  ['ifo sell-first', 'ifo', 'sell', { amount: '4', price: '2.7', 'exit.takeProfit': '2.4', 'exit.stop': '2.9' }],
  ['repeat ifd unlimited', 'repeatIfd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6' }],
  ['repeat ifd x3', 'repeatIfd', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', 'repeat.count': '3' }],
  ['repeat ifo', 'repeatIfo', 'buy', { amount: '4', price: '2.3', 'exit.takeProfit': '2.6', 'exit.stop': '2.1' }],
];

