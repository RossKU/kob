import { describe, expect, it } from 'vitest';
import type { PlanIssue } from '../../kob/plan-types';
import type { OrderState } from '../../kob/types';
import { DEFAULT_MIN_ORDER_VALUE_SOMPI, isTicketNote, listingValueOf, minOrderValueOf, minValueNote, ticketNotes } from './ticket-notes';

const w = (code: string, message = code): PlanIssue => ({ code, severity: 'warning', message });
const e = (code: string): PlanIssue => ({ code, severity: 'error', message: code });

describe('ticket notes', () => {
  it('the warnings the ticket shows next to the button', () => {
    for (const code of [
      'PRICE_AGGRESSIVE_VS_MARKET', 'MARKET_REFERENCE_DIVERGES', 'SLIPPAGE_HIGH', 'DAY_ORDER_ENDS_SOON', 'MARKETABLE_LIMIT_FILLS_AT_LIMIT', 'STOP_EVIDENCE_GENESIS_UNVERIFIED',
      'COND_STOP_ALREADY_REACHED', 'COND_TP_CROSSES', 'COND_ENTRY_CROSSES', 'COND_TP_NOT_PROFITABLE',
      'PAIR_PRICE_AGGRESSIVE_VS_MARKET', 'PAIR_MARKET_REFERENCE_DIVERGES', 'PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT', 'PAIR_TP_CROSSES',
    ]) expect(isTicketNote(w(code))).toBe(true);
  });

  it('an error is never a note (it is the red line that disables the button), nor are the warnings the Fills line already says', () => {
    expect(isTicketNote(e('PRICE_AGGRESSIVE_VS_MARKET'))).toBe(false);
    expect(isTicketNote(e('COND_TP_NOT_PROFITABLE'))).toBe(false);
    for (const code of ['MARKET_ONE_TX_LIMIT', 'IOC_ONE_TX_LIMIT', 'FILLS_BUDGET_SHORT', 'MARKET_DEPTH_INSUFFICIENT']) expect(isTicketNote(w(code))).toBe(false);
    expect(isTicketNote({ code: 'PRICE_AGGRESSIVE_VS_MARKET', severity: 'info' })).toBe(false);
  });

  it('keeps the planner order and drops a repeat of the same finding (an entry and an exit leg raising one)', () => {
    const issues = [w('SLIPPAGE_HIGH'), e('AMOUNT_NOT_POSITIVE'), w('COND_TP_CROSSES', 'a'), w('COND_TP_CROSSES', 'a'), w('COND_TP_CROSSES', 'b'), w('CARRIERS_DOMINATE')];
    expect(ticketNotes(issues).map((i) => `${i.code}:${i.message}`)).toEqual(['SLIPPAGE_HIGH:SLIPPAGE_HIGH', 'COND_TP_CROSSES:a', 'COND_TP_CROSSES:b']);
  });
});

// ------------------------------------------------------------------------------------------------ minimum order value

const KAS = 100_000_000n;
/** The states are hand-made: only the fields the listing rule reads (amountLeft, price, scale) matter, so this needs no wasm. */
const ask = (amountLeft: string, price: string, scale: string, kind = 'KobAsk'): OrderState => ({ kind, state: { amountLeft, price, scale } }) as unknown as OrderState;
const bid = (price: string, scale: string, kind = 'KobBid'): OrderState => ({ kind, state: { price, scale } }) as unknown as OrderState;
const plan = (state: OrderState, value: string | bigint = '1000000000') => ({ states: [state], request: { action: 'createOrder', order: state, value: String(value), funding: [] } }) as never;

describe('minimum order value: the value the listing rule measures', () => {
  it('the default is the executor default, 1 KAS', () => {
    expect(DEFAULT_MIN_ORDER_VALUE_SOMPI).toBe(KAS);
  });

  it('a sell is its amount at its quote, rounded down (8-decimal token)', () => {
    // 0.4 tokens at 2.5 KAS per token = 1 KAS exactly
    expect(listingValueOf(ask('40000000', '250000000', '100000000'), 5n * KAS)).toBe(KAS);
    // one base unit less: 0.39999999 * 2.5 = 0.999999975 KAS, floored to 99999997 sompi
    expect(listingValueOf(ask('39999999', '250000000', '100000000'), 5n * KAS)).toBe(99_999_997n);
  });

  it('a sell of a 3-decimal token counts per 1000 base units', () => {
    // 0.4 tokens (400 base units) at 2.5 KAS per token
    expect(listingValueOf(ask('400', '250000000', '1000'), KAS)).toBe(KAS);
    expect(listingValueOf(ask('399', '250000000', '1000'), KAS)).toBe(99_750_000n);
  });

  it('a KRON sell is measured the same way', () => {
    expect(listingValueOf(ask('399', '250000000', '1000', 'KobAskKron'), KAS)).toBe(99_750_000n);
  });

  it('a bid is its KAS escrow, whatever its price', () => {
    expect(listingValueOf(bid('250000000', '1000'), 3n * KAS)).toBe(3n * KAS);
    expect(listingValueOf(bid('250000000', '1000', 'KobBidKron'), 99_999_999n)).toBe(99_999_999n);
  });

  it('conditional, if-done and pair orders are not measured here', () => {
    for (const kind of ['KobCondAsk', 'KobCondBid', 'KobIfdAsk', 'KobIfdBid', 'KobPair', 'KobCondPair', 'KobIfdPair', 'KobCondAskKron']) {
      expect(listingValueOf(ask('400', '250000000', '1000', kind), KAS), kind).toBeNull();
    }
  });

  it('an unreadable state has no value', () => {
    expect(listingValueOf(ask('400', '250000000', '0'), KAS)).toBeNull();
    expect(listingValueOf(ask('abc', '250000000', '1000'), KAS)).toBeNull();
    expect(listingValueOf(ask('400', '-1', '1000'), KAS)).toBeNull();
  });
});

describe('minimum order value: the note', () => {
  const min = DEFAULT_MIN_ORDER_VALUE_SOMPI;

  it('a sell below the minimum is noted with its value and the minimum', () => {
    expect(minValueNote(plan(ask('39999999', '250000000', '100000000')), min)).toEqual({ value: 99_999_997n, min });
  });

  it('a sell exactly at the minimum is listed: no note', () => {
    expect(minValueNote(plan(ask('40000000', '250000000', '100000000')), min)).toBeNull();
  });

  it('a sell above the minimum: no note', () => {
    expect(minValueNote(plan(ask('50000000', '250000000', '100000000')), min)).toBeNull();
    expect(minValueNote(plan(ask('1000', '250000000', '1000')), min)).toBeNull();
  });

  it('a bid is measured by its escrow (the order UTXO value), below / at / above', () => {
    const b = bid('250000000', '100000000');
    expect(minValueNote(plan(b, 99_999_999n), min)).toEqual({ value: 99_999_999n, min });
    expect(minValueNote(plan(b, KAS), min)).toBeNull();
    expect(minValueNote(plan(b, 2n * KAS), min)).toBeNull();
  });

  it('follows another minimum', () => {
    const s = plan(ask('40000000', '250000000', '100000000'));
    expect(minValueNote(s, 2n * KAS)).toEqual({ value: KAS, min: 2n * KAS });
    expect(minValueNote(s, 0n)).toBeNull();
  });

  it('no note without a plan, with several orders, without a request or for an order the rule does not measure', () => {
    expect(minValueNote(null, min)).toBeNull();
    const s = ask('1', '1', '1000');
    expect(minValueNote({ states: [s, s], request: null } as never, min)).toBeNull();
    expect(minValueNote({ states: [s], request: null } as never, min)).toBeNull();
    expect(minValueNote(plan(ask('1', '1', '1000', 'KobCondAsk')), min)).toBeNull();
    expect(minValueNote(plan(ask('1', '1', '1000', 'KobPair')), min)).toBeNull();
  });
});

describe('minimum order value: where the minimum comes from', () => {
  it('is the executor default when /v1/health does not report one', () => {
    expect(minOrderValueOf(null)).toEqual({ sompi: KAS, source: 'default' });
    expect(minOrderValueOf({ state: 'following', ok: true })).toEqual({ sompi: KAS, source: 'default' });
    expect(minOrderValueOf({ min_order_value_sompi: 'x' })).toEqual({ sompi: KAS, source: 'default' });
    expect(minOrderValueOf({ min_order_value_sompi: -1 })).toEqual({ sompi: KAS, source: 'default' });
  });

  it('reads the optional field a future executor may report (string or safe integer, sompi)', () => {
    expect(minOrderValueOf({ min_order_value_sompi: '250000000' })).toEqual({ sompi: 250_000_000n, source: 'indexer' });
    expect(minOrderValueOf({ min_order_value_sompi: 0 })).toEqual({ sompi: 0n, source: 'indexer' });
  });
});
