// An order's details for a stepped sell curve (KobCurveAsk). They show the remaining levels as a
// step line with the sold part muted, the first, last and now price, the levels left and what the
// rest pays if it sells.
import { t } from '../../i18n';
import { curveRestKas, curveSteps, type CurveStep } from '../../kob/curve';
import type { CurveFacts } from '../../kob/order-facts';
import { Amount, CurveGraph, KeyValueList } from '../kit';

export interface CurveDetailsProps {
  id: string;
  curve: CurveFacts;
  amountLeft: bigint | null;
  scale: bigint;
  decimals: number;
}

/**
 * Sompi the levels still for sale pay if they all sell. Each level is rounded up, as the covenant
 * pays the maker.
 */
export const restProceeds = (c: CurveFacts, amountLeft: bigint, scale: bigint): bigint => curveRestKas(c.levelSize, c.table, c.total, amountLeft, scale);

/**
 * The steps lifted onto a floor a quarter of the table's range under its lowest price, never below
 * zero, so the rise shows.
 */
export function graphSteps(steps: CurveStep[]): CurveStep[] {
  if (!steps.length) return steps;
  const lo = steps.reduce((m, x) => (x.price < m ? x.price : m), steps[0]!.price);
  const hi = steps.reduce((m, x) => (x.price > m ? x.price : m), steps[0]!.price);
  const pad = (hi - lo) / 4n;
  const floor = hi > lo && lo > pad ? lo - pad : 0n;
  return floor > 0n ? steps.map((x) => ({ ...x, price: x.price - floor })) : steps;
}

export function CurveDetails(p: CurveDetailsProps) {
  const c = p.curve;
  const left = p.amountLeft ?? 0n;
  // The graph spans the table's own range. A gentle curve, 1.04x, would otherwise be a flat line
  // over a zero baseline. Prices above a floor under the first are shown.
  const steps = graphSteps(curveSteps(c.levelSize, c.table, c.total, left));
  const price = (v: bigint | null | undefined) => (v === null || v === undefined ? '—' : <Amount kind="price" value={v} decimals={p.decimals} scale={p.scale} group />);
  return (
    <div class="stack-sm or-curve" data-testid={`order-curve-${p.id}`}>
      <CurveGraph steps={steps} label={t('curve.details.graph')} axis={[t('curve.details.axisStart'), t('curve.details.axisEnd')]} data-testid="order-curve-graph" />
      <KeyValueList
        compact
        flow
        items={[
          { label: t('curve.details.first'), value: price(c.table[0]), 'data-testid': 'order-curve-first' },
          { label: t('curve.details.last'), value: price(c.table[c.levels - 1]), 'data-testid': 'order-curve-last' },
          { label: t('curve.details.left'), value: t('curve.details.leftValue', { left: Math.max(0, c.levels - c.level), levels: c.levels }), 'data-testid': 'order-curve-left' },
          { label: t('curve.details.ifSold'), value: <Amount kind="kas" value={restProceeds(c, left, p.scale)} group />, 'data-testid': 'order-curve-rest', show: left > 0n },
        ]}
      />
      {c.unbound ? <p class="small muted" data-testid="order-curve-unbound">{t('curve.details.unbound')}</p> : null}
    </div>
  );
}
