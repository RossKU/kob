import { t } from '../../i18n';
import type { EventView } from '../../data/indexer-types';
import type { Clock } from '../../kob/plan-types';
import type { Hex } from '../../kob/types';
import type { Position } from '../../kob/positions';
import { longId, type TokenInfo } from '../../kob/registry';
import { Badge, Button } from '../kit';
import { OrderRow, type OrderRowActions } from './OrderRow';
import type { OrderRowModel } from './orders-model';
import { summarizePosition } from './position-model';
import { PositionSummaryPanel } from './PositionSummary';
import './positions.css';

export interface PositionCardProps {
  position: Position;
  rows: ReadonlyMap<string, OrderRowModel>;
  tokenOf(id: string | null): TokenInfo | undefined;
  canSign: boolean;
  actions: OrderRowActions;
  onCancelPosition(position: Position): void;
  /**
   * close: cancel the position's live orders, then unwind at market: a buy-first position sells the released tokens, a sell-first one buys
   * back the amount it sold (absent: no close button)
   */
  onClosePosition?(position: Position): void;
  busy: string | null;
  /** wall-clock mapping of DAA scores (repeat "until" date) */
  clock?: Clock | null;
  /** loads the fill events of one order; absent (no indexer): no fills section */
  loadEvents?: (id: Hex, signal: AbortSignal) => Promise<EventView[]>;
}

/** An if-done / OCO-entry / repeat position: the entry with its exits, one cancel for all of them (ONE transaction where possible). */
export function PositionCard(props: PositionCardProps) {
  const p = props.position;
  const entryRow = p.entry ? props.rows.get(p.entry.covenant_id) : undefined;
  const exitRows = p.exits.map((x) => props.rows.get(x.covenant_id)).filter((r): r is OrderRowModel => !!r);
  const sum = summarizePosition(p, props.rows, props.clock ?? null);
  const tone = sum.phase === 'closed' || sum.phase === 'cancelled' ? 'neutral' : sum.phase === 'repeat-waiting' ? 'info' : 'ok';
  const token = props.tokenOf(sum.token);
  // a pair position (KobIfdPair entry and its KobCondPair exits): prices in the quote token B, the title names the pair
  const pairOf = entryRow?.pair ?? exitRows.find((r) => r.pair)?.pair ?? null;
  const pair = pairOf ? { quote: props.tokenOf(pairOf.quote), quoteId: pairOf.quote } : null;
  const typeKey = entryRow?.typeKey && entryRow.typeKey !== 'unknown' ? entryRow.typeKey : null;
  return (
    <div
      class={`position pc pc-${p.side}${sum.phase === 'closed' || sum.phase === 'cancelled' ? ' pc-ended' : ''}`}
      data-testid={`position-${p.id}`}
      data-kind={p.kind}
      data-status={p.status}
      data-phase={sum.phase}
    >
      <div class="position-head pc-head">
        <div class="pc-head-main">
          <strong class="pc-title" data-testid="position-title">
            {typeKey === 'ifo' || typeKey === 'ifd' || typeKey === 'repeat' ? t(`orders.type.${typeKey}`) : t(`orders.position.${p.kind}`)}
          </strong>
          {token && pair ? (
            <a class="pc-token" href={`#/market/${token.covenantId}/${pair.quoteId}`} data-testid="position-token">
              {`${token.ticker}/${pair.quote?.ticker ?? longId(pair.quoteId)}`}
            </a>
          ) : token ? (
            <a class="pc-token" href={`#/market/${token.covenantId}`} data-testid="position-token">
              {token.ticker}
            </a>
          ) : null}
          <span class={`pc-chip pc-chip-${p.side}`}>{t(`orders.position.first.${p.side}`)}</span>
          <Badge tone={tone} class="pc-phase" data-testid="position-status">
            {t(`orders.position.phase.${sum.phase}`)}
          </Badge>
        </div>
        {p.cancelIds.length > 0 ? (
          <Button
            small
            variant="danger"
            class="pc-cancel"
            disabled={!props.canSign || props.busy === p.id}
            title={props.canSign ? undefined : t('orders.actions.needWallet')}
            onClick={() => props.onCancelPosition(p)}
            data-testid={`position-cancel-${p.id}`}
          >
            {t('orders.position.cancelAll', { count: p.cancelIds.length })}
          </Button>
        ) : null}
        {/* the market close sells / buys back for KAS: a pair position is closed by cancelling it and trading on its pair page */}
        {props.onClosePosition && p.cancelIds.length > 0 && sum.token && !pair ? (
          <Button
            small
            class="pc-close"
            disabled={!props.canSign || props.busy === p.id}
            title={props.canSign ? t(p.side === 'buy' ? 'orders.position.closeHint' : 'orders.position.closeHintSell') : t('orders.actions.needWallet')}
            onClick={() => props.onClosePosition!(p)}
            data-testid={`position-close-${p.id}`}
          >
            {t('orders.position.close')}
          </Button>
        ) : null}
      </div>
      {/* the realised PnL is in KAS from the fills' KAS prices: a pair fill has none (it never makes a price), so a pair position has no PnL line */}
      <PositionSummaryPanel position={p} summary={sum} token={token} pair={pair} {...(props.loadEvents && !pair ? { loadEvents: props.loadEvents } : {})} />
      <div class="pc-members stack-sm">
        {p.orphanExits ? <p class="small muted pc-note">{t('orders.position.orphan')}</p> : null}
        {entryRow ? (
          <OrderRow row={entryRow} token={props.tokenOf(entryRow.token)} canSign={props.canSign} actions={props.actions} busy={props.busy === entryRow.id} {...(props.loadEvents ? { loadEvents: props.loadEvents } : {})} />
        ) : null}
        {exitRows.length ? (
          <div class="position-exits stack-sm" data-testid="position-exits">
            <div class="pc-section-label">{t('orders.position.exits')}</div>
            {exitRows.map((r) => (
              <OrderRow key={r.id} row={r} token={props.tokenOf(r.token)} canSign={props.canSign} actions={props.actions} busy={props.busy === r.id} nested {...(props.loadEvents ? { loadEvents: props.loadEvents } : {})} />
            ))}
          </div>
        ) : null}
      </div>
    </div>
  );
}
