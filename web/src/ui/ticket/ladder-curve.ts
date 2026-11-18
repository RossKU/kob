// A sell ladder as one curve order (KobCurveAsk): the ladder's prices are the curve's table, and
// its level amount is the level size. Both spacings map: percent steps, and even steps to an end
// price, as long as the prices never fall. One order, one UTXO, one transaction.
import { t } from '../../i18n';
import type { CurveIntent, LimitIntent } from '../../kob/intent-simple';
import type { Intent } from '../../kob/plan';
import type { StepperItem } from '../kit/stepper-model';
import type { LadderResult } from './ladder';

/**
 * The curve of a sell limit ladder, or null when it is not one (a buy, a falling price, levels of
 * other sizes).
 */
export function curveOfLadder(ladder: LadderResult, typed: Intent | null): CurveIntent | null {
  if (!ladder.ok || ladder.levels.length < 2 || !typed || typed.type !== 'limit' || typed.side !== 'sell') return null;
  const first = ladder.levels[0]!.intent as LimitIntent;
  const table = ladder.levels.map((l) => l.entryPrice);
  for (let i = 1; i < table.length; i++) if (table[i]! < table[i - 1]!) return null;
  if (ladder.levels.some((l) => (l.intent as LimitIntent).amount !== first.amount)) return null;
  return {
    type: 'curve', side: 'sell', amount: first.amount * BigInt(table.length), levelSize: first.amount, table,
    ...(first.tip !== undefined ? { tip: first.tip } : {}), ...(first.minFill !== undefined ? { minFill: first.minFill } : {}),
  };
}

/**
 * The stepper of a ladder placed as separate orders: one step per level, with `at` the one being
 * placed (`levels` means all done).
 */
export function ladderStepItems(s: { at: number; levels: number; failed?: boolean }, running: boolean): StepperItem[] {
  return Array.from({ length: s.levels }, (_, i) => {
    const label = t('ticket.ladder.step', { level: i + 1 });
    const testid = `order-ladder-step-${i + 1}`;
    if (i < s.at) return { label, state: 'done', testid };
    if (i === s.at && s.failed) return { label, state: 'failed', testid };
    if (i === s.at && running) return { label, state: 'current', busy: true, testid };
    return { label, state: 'waiting', testid };
  });
}
