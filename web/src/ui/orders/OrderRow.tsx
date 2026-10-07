import { pairRoute, routeToHash } from '../../app/router';
import { useServices } from '../../app/context';
import { useTxUrl } from '../../app/explorer';
import { formatDateTime, formatNumber, t } from '../../i18n';
import { unitsText } from '../../kob/pair';
import { statePriceToTokenPrice } from '../../kob/units';
import { formatJst, formatUtc } from '../../kob/daa';
import type { TokenInfo } from '../../kob/registry';
import { Amount, Badge, Banner, Button, CopyText, KeyValueList, tickKasFraction, tickPriceFraction, type Tone } from '../kit';
import { labelState, tokenLabel } from '../market/token-model';
import type { EventView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { OrderFills, filledAmountOf } from './OrderFills';
import { GTC_RULE, type OrderRowModel, type OrderStatusUi } from './orders-model';

const STATUS_TONE: Record<OrderStatusUi, Tone> = { open: 'ok', partial: 'info', filled: 'neutral', cancelled: 'neutral', refunded: 'neutral', killed: 'neutral', closed: 'neutral', unknown: 'warn' };

export interface OrderRowActions {
  onCancel(row: OrderRowModel): void;
  onAmend(row: OrderRowModel): void;
  onRefund(row: OrderRowModel): void;
  /** the maker's sweep of the order's strays in place (the order continues unchanged) */
  onSweep(row: OrderRowModel): void;
}

export interface OrderRowProps {
  row: OrderRowModel;
  token: TokenInfo | undefined;
  /** the wallet is connected on the app's network: signing actions are enabled */
  canSign: boolean;
  actions: OrderRowActions;
  /** an action on this order is being prepared (the buttons show a spinner) */
  busy?: boolean;
  /** rendered as an exit of a position (indented) */
  nested?: boolean;
  /** loads the order's events (fill history on demand); absent (no indexer): no fill history */
  loadEvents?: (id: Hex, signal: AbortSignal) => Promise<EventView[]>;
}

const localDate = (unix: bigint): string => formatDateTime(Number(unix));

/** The fixed decimals of a price shown in every order of a market (from the token's tick): `{ fraction }`, or nothing when the market has no tick. */
const fixed = (fraction: number | null): { fraction?: number } => (fraction === null ? {} : { fraction });

const shortId = (id: string): string => `${id.slice(0, 4)}…${id.slice(-4)}`;

/**
 * A price of a pair order: B base units per `row.scale` base units of A, shown per whole A (`value x 10^decimals(A) / scale`) when A is known, in B
 * (its decimals) when B is known; raw base units of B when B is not in the registry, per `scale` base units of A when A is not.
 */
function pairPriceText(value: bigint, row: OrderRowModel, a: TokenInfo | undefined, b: TokenInfo | undefined): string {
  const aName = a?.ticker ?? (row.token ? shortId(row.token) : '?');
  const perA = a && row.scale > 0n ? statePriceToTokenPrice(value, a.decimals, row.scale, 'nearest') : value;
  const unitA = a && row.scale > 0n ? aName : `${aName} ${t('orders.perScale', { scale: row.scale.toString() })}`;
  if (!b) return `${perA.toString()} ${t('common.baseUnits')} ${shortId(row.pair!.quote)}/${unitA}`;
  return `${unitsText(perA, b.decimals, ',')} ${b.ticker}/${unitA}`;
}

/**
 * A state price of the order (sompi per whole token of `row.scale` base units) as KAS per whole token; per `scale` base units without the token. A pair
 * order's prices are in its quote token B per whole A (`quote`).
 */
function PriceCell(p: { row: OrderRowModel; token: TokenInfo | undefined; quote?: TokenInfo | undefined; value: bigint | null; testid?: string }) {
  const tk = p.token;
  if (p.value === null) return <>{'—'}</>;
  if (p.row.pair) return <span data-testid={p.testid} data-unit="pair">{pairPriceText(p.value, p.row, tk, p.quote)}</span>;
  return (
    <span data-testid={p.testid}>
      {tk ? (
        <Amount kind="price" value={p.value} decimals={tk.decimals} scale={p.row.scale} unit={`KAS/${tk.ticker}`} {...fixed(tickPriceFraction(tk.tick, tk.decimals, p.row.scale))} group />
      ) : (
        <Amount kind="kas" value={p.value} unit={`KAS ${t('orders.perScale', { scale: p.row.scale.toString() })}`} {...fixed(tickKasFraction(null))} />
      )}
    </span>
  );
}

function ExpiryText({ row }: { row: OrderRowModel }) {
  const e = row.expiry;
  if (e.kind === 'day' && e.deadlineUnix !== null) {
    return <span data-testid="order-expiry">{t('orders.expiry.day', { utc: formatUtc(e.deadlineUnix), jst: formatJst(e.deadlineUnix) })}</span>;
  }
  if (e.kind === 'gtc') {
    return <span data-testid="order-expiry">{t('orders.expiry.gtc', { days: GTC_RULE.days })}</span>;
  }
  if (e.approxUnix !== null) return <span data-testid="order-expiry">{t('orders.expiry.around', { date: localDate(e.approxUnix) })}</span>;
  if (e.daa !== null) return <span data-testid="order-expiry">{t('orders.expiry.daa', { daa: formatNumber(e.daa) })}</span>;
  return <>{'—'}</>;
}

/** The pair, what the order holds of each token, and the trigger rule of a pair stop (the B side comes from the registry; else its short id). */
function pairItems(row: OrderRowModel, a: TokenInfo | undefined, b: TokenInfo | undefined) {
  const x = row.pair!;
  const aName = a?.ticker ?? (row.token ? shortId(row.token) : '?');
  const bName = b?.ticker ?? shortId(x.quote);
  const aText = (v: bigint): string => (a ? `${unitsText(v, a.decimals, ',')} ${a.ticker}` : `${v.toString()} ${t('common.baseUnits')}`);
  const bText = (v: bigint): string => (b ? `${unitsText(v, b.decimals, ',')} ${b.ticker}` : `${v.toString()} ${t('common.baseUnits')}`);
  const r = x.trigger;
  return [
    {
      label: t('orders.pair.pair'),
      value: <a href={routeToHash(pairRoute(x.base, x.quote))} data-testid="order-pair">{t('orders.pair.pairValue', { base: aName, quote: bName })}</a>,
    },
    { label: t('orders.pair.escrowA'), value: <span data-testid="order-pair-escrow-a">{aText(x.escrowA)}</span>, show: x.escrowA > 0n },
    {
      label: t(x.kind === 'KobIfdPair' && row.side === 'sell' ? 'orders.pair.prefundB' : 'orders.pair.escrowB'),
      value: <span data-testid="order-pair-escrow-b">{bText(x.escrowB)}</span>,
      show: x.escrowB > 0n,
    },
    { label: t('orders.pair.tip'), value: t('orders.pair.tipValue', { kas: `${unitsText(x.tipKas, 8, ',')} KAS`, base: aName }), show: x.tipKas > 0n },
    {
      label: t('orders.pair.quoteNow'),
      value: <span data-testid="order-pair-now">{pairPriceText(x.quoteNow ?? 0n, row, a, b)}</span>,
      show: x.quoteNow !== null && row.live,
    },
    {
      label: t('orders.pair.trigger'),
      value: (
        <span data-testid="order-pair-trigger">
          {r ? t(r.direction === 'fallsTo' ? 'orders.pair.triggerSell' : 'orders.pair.triggerBuy', { stop: pairPriceText(BigInt(r.stop), row, a, b), base: aName, quote: bName, seconds: String(Number(r.minRestDaa) / 10) }) : ''}
        </span>
      ),
      show: !!r && row.live,
    },
  ];
}

/** One order: type in plain words, side, all-in price, amount, status, expiry / renewal / refund, custody flag, placement tx, and the actions. */
export function OrderRow(props: OrderRowProps) {
  const { row, token, canSign, actions } = props;
  const { registry, config } = useServices();
  const txUrl = useTxUrl();
  const placementUrl = txUrl(row.placementTx);
  const bToken = row.pair ? registry.byCovenantId.get(row.pair.quote) : undefined;
  const tk = token;

  const disabledReason = !canSign ? t('orders.actions.needWallet') : undefined;
  const cancelHint = row.canCancel ? undefined : row.cancelBlocked ? t(`orders.cancelBlocked.${row.cancelBlocked}`) : t('orders.actions.cannotCancel');
  // strays of a live order the wallet can spend: the maker's sweep returns them while the order lives on
  const canSweep = row.live && row.canCancel && row.strayCount > 0;

  return (
    <div class={`order-card${props.nested ? ' nested' : ''}`} data-testid={`order-row-${row.id}`} data-status={row.status} data-type={row.typeKey} data-side={row.side} data-live={row.live ? '1' : '0'}>
      <div class="row-between">
        <div class="row" style="gap:6px">
          <strong data-testid={`order-type-${row.id}`}>{t(`orders.type.${row.typeKey}`)}</strong>
          {placementUrl ? (
            <a class="explorer-link small" href={placementUrl} target="_blank" rel="noopener noreferrer" title={t('orders.explorerHint')} aria-label={t('orders.explorerHint')} data-testid={`order-explorer-${row.id}`}>
              {t('orders.explorer')}
            </a>
          ) : null}
          <Badge tone={row.side === 'buy' ? 'ok' : 'bad'} data-testid="order-side">{t(`common.side.${row.side}`)}</Badge>
          <Badge tone={STATUS_TONE[row.status]} data-testid="order-status">{t(`orders.status.${row.status}`)}</Badge>
          {row.expired && row.live ? <Badge tone="warn" data-testid="order-expired">{t('orders.expired')}</Badge> : null}
          {row.armed === true ? <Badge tone="info" data-testid="order-armed">{t('orders.armed')}</Badge> : null}
          {row.armed === false && row.live ? <Badge tone="neutral">{t('orders.waitingTrigger')}</Badge> : null}
          {row.startsAtDaa !== null ? <Badge tone="neutral">{t('orders.notStarted')}</Badge> : null}
          {row.unfundable ? <Badge tone="warn" data-testid="order-unfundable-badge">{t('orders.unfundable.badge')}</Badge> : null}
          {row.possiblyFrozen ? <Badge tone="bad" title={t('orders.frozen.hint')} data-testid="order-frozen-badge">{t('orders.frozen.badge')}</Badge> : null}
          {row.custodyOk === false ? <Badge tone="bad" data-testid="order-custody-bad">{t('orders.custodyBad')}</Badge> : null}
          {row.oldTemplate ? <Badge tone="warn" title={t('orders.oldTemplate.hint')} data-testid="order-old-template">{t('orders.oldTemplate.badge')}</Badge> : null}
          {row.source !== 'indexer' ? <Badge tone="warn" title={t(`orders.source.${row.source}Hint`)} data-testid="order-source">{t(`orders.source.${row.source}`)}</Badge> : null}
        </div>
        <div class="small muted">
          {tk && row.pair ? (
            <a href={routeToHash(pairRoute(row.pair.base, row.pair.quote))} data-testid="order-token">{`${tk.ticker}/${bToken?.ticker ?? shortId(row.pair.quote)}`}</a>
          ) : tk ? (
            <a href={`#/market/${tk.covenantId}`} data-testid="order-token">{tokenLabel(tk, t(`market.state.${labelState(tk)}`, { hash: tk.customRegistry ?? '' }))}</a>
          ) : row.token ? (
            <span>{t('orders.unknownToken', { id: `${row.token.slice(0, 4)}…${row.token.slice(-4)}` })}</span>
          ) : null}
        </div>
      </div>

      {row.unfundable ? (
        <Banner
          tone="warn"
          data-testid="order-unfundable"
          actions={
            row.canCancel ? (
              <Button small variant="danger" disabled={!canSign || props.busy} title={disabledReason} onClick={() => actions.onCancel(row)} data-testid={`order-unfundable-refund-${row.id}`}>
                {t('orders.unfundable.action')}
              </Button>
            ) : undefined
          }
        >
          {t(row.canAmend ? 'orders.unfundable.bodyAmend' : 'orders.unfundable.body')}
        </Banner>
      ) : null}
      {row.cancelBlocked && row.cancelBlocked !== 'old-template' ? (
        <Banner tone="warn" data-testid="order-cancel-blocked">
          {t(`orders.cancelBlocked.${row.cancelBlocked}`)}
        </Banner>
      ) : null}
      {row.possiblyFrozen ? (
        <Banner tone="warn" title={t('orders.frozen.title')} data-testid="order-frozen">
          {t('orders.frozen.body')}
        </Banner>
      ) : null}

      <KeyValueList
        compact
        items={[
          ...(row.pair ? pairItems(row, tk, bToken) : []),
          // an auction's bound is its END price (the worst it can fill at), never labelled a limit; its start is only the expected price
          {
            label: t(row.auctionStart !== null ? 'orders.worstPrice' : 'orders.limitPrice'),
            value: <PriceCell row={row} token={tk} quote={bToken} value={row.price} />,
            'data-testid': 'order-price',
            show: row.price !== null || row.stopPrice === null,
          },
          { label: t('orders.startPrice'), value: <PriceCell row={row} token={tk} quote={bToken} value={row.auctionStart} />, 'data-testid': 'order-start-price', show: row.auctionStart !== null },
          { label: t('orders.stopPrice'), value: <PriceCell row={row} token={tk} quote={bToken} value={row.stopPrice} />, 'data-testid': 'order-stop', show: row.stopPrice !== null },
          { label: t('orders.allIn'), value: <PriceCell row={row} token={tk} value={row.allInPrice} />, 'data-testid': 'order-allin', show: row.allInPrice !== null },
          {
            label: t('orders.auctionNow'),
            value: <PriceCell row={row} token={tk} value={row.auction?.currentPrice ?? null} />,
            show: row.auction !== null && !row.auction.complete && row.live && !row.pair,
          },
          {
            label: t('orders.amount'),
            value: (
              <span data-testid="order-amount">
                {row.amountLeft !== null ? (
                  <>
                    {row.amountEstimated ? <span class="est" title={t('market.book.estimatedNote')}>~</span> : null}
                    {tk ? <Amount kind="token" value={row.amountLeft} decimals={tk.decimals} group /> : row.amountLeft.toString()}
                    {row.amountTotal !== null ? <> / {tk ? <Amount kind="token" value={row.amountTotal} decimals={tk.decimals} group /> : row.amountTotal.toString()}</> : null}{' '}
                    {tk ? tk.ticker : t('common.baseUnits')}
                  </>
                ) : (
                  <>{t('orders.budgetOrder')}</>
                )}
              </span>
            ),
          },
          { label: t('orders.expires'), value: <ExpiryText row={row} />, show: row.live },
          {
            label: t('orders.renew'),
            value: (
              <span data-testid="order-renewal">
                {row.renewalUnix !== null ? t('orders.renewBefore', { date: localDate(row.renewalUnix), day: GTC_RULE.renewDay, days: GTC_RULE.days }) : ''}
                {row.renewalDue ? <Badge tone="warn" data-testid="order-renewal-due">{t('orders.renewNow')}</Badge> : null}
              </span>
            ),
            show: row.renewalUnix !== null,
          },
          {
            label: t('orders.refundFrom'),
            value: <span data-testid="order-refund-from">{row.refundable ? t('orders.refundableNow') : t('orders.refundDaa', { daa: formatNumber(row.refundDueDaa ?? 0n) })}</span>,
            show: row.live && row.refundDueDaa !== null && (row.expired || row.refundable),
          },
          {
            label: t('orders.strays'),
            value: <span data-testid="order-strays">{t(canSweep ? 'orders.straysValueSweep' : 'orders.straysValue', { count: row.strayCount })}</span>,
            show: row.strayCount > 0,
          },
          { label: t('orders.note'), value: row.label ?? '', show: !!row.label },
          {
            label: t('orders.placementTx'),
            value: row.placementTx ? <CopyText value={row.placementTx} head={8} tail={6} href={placementUrl} data-testid={`order-tx-${row.id}`} /> : '—',
          },
          { label: t('orders.orderId'), value: <CopyText value={row.id} head={8} tail={6} data-testid={`order-id-${row.id}`} /> },
        ]}
      />

      {props.loadEvents && filledAmountOf(row) > 0n ? <OrderFills row={row} token={tk} quote={bToken} network={config.network} loadEvents={props.loadEvents} /> : null}

      {row.live || row.status === 'unknown' ? (
        <div class="order-actions" style="margin-top:8px">
          <Button
            small
            variant="danger"
            disabled={!canSign || !row.canCancel || props.busy}
            title={disabledReason ?? cancelHint}
            onClick={() => actions.onCancel(row)}
            data-testid={`order-cancel-${row.id}`}
          >
            {t('orders.cancel')}
          </Button>
          {canSweep ? (
            <Button small disabled={!canSign || props.busy} title={disabledReason ?? t('orders.sweep.hint')} onClick={() => actions.onSweep(row)} data-testid={`order-sweep-${row.id}`}>
              {t('orders.sweep.action')}
            </Button>
          ) : null}
          {row.canAmend ? (
            <Button small disabled={!canSign || props.busy} title={disabledReason} onClick={() => actions.onAmend(row)} data-testid={`order-replace-${row.id}`}>
              {t('orders.replace')}
            </Button>
          ) : null}
          {row.canAmend && row.renewalDue ? (
            <Button small variant="primary" disabled={!canSign || props.busy} title={disabledReason} onClick={() => actions.onAmend(row)} data-testid={`order-renew-${row.id}`}>
              {t('orders.renewAction')}
            </Button>
          ) : null}
          {row.canRefund ? (
            <Button small disabled={!canSign || props.busy} title={disabledReason} onClick={() => actions.onRefund(row)} data-testid={`order-refund-${row.id}`}>
              {t('orders.refund')}
            </Button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
