import { describe, expect, it } from 'vitest';
import type { PlanIssue } from '../../kob/plan-types';
import { isTicketNote, ticketNotes } from './ticket-notes';

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
