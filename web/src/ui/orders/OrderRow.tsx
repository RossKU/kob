import { useEffect, useState } from 'preact/hooks';
import { pairRoute, routeToHash } from '../../app/router';
import { useServices } from '../../app/context';
import { useTxUrl } from '../../app/explorer';
import { formatDateTime, formatNumber, t, RAW_TOKEN_DECIMALS } from '../../i18n';
import { unitsText } from '../../kob/pair';
import { formatKas, formatPricePerToken, formatUnits, quoteOf, statePriceToTokenPrice } from '../../kob/units';
import { daaToUnix, formatLocal, formatUtc } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { longId, type TokenInfo } from '../../kob/registry';
import { Amount, Badge, Banner, Button, CopyText, KeyValueList, RawDetails, tickPriceFraction, type Tone } from '../kit';
import { tokenLabel } from '../market/token-model';
import { TokenNote } from '../market/TokenNote';
import { IdentityBlock } from '../market/Fingerprint';
import { displayToken } from '../../app/token-display';
import type { EventView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { OrderFills, filledAmountOf } from './OrderFills';
import { GTC_RULE, type OrderRowModel, type OrderStatusUi } from './orders-model';
import { unlistedHint, unlistedText } from './unlisted';
import './order-line.css';

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
  /** wall-clock mapping of DAA scores: the line's placement time (none: no time) */
  clock?: Clock | null;
}

/** An order that needs the reader: its details open by themselves (a warning, a renewal, an expiry, a source other than the indexer). */
export function needsAttention(row: OrderRowModel): boolean {
  return (
    row.unfundable || row.possiblyFrozen || !!row.unlisted || (!!row.cancelBlocked && row.cancelBlocked !== 'old-template') || row.custodyOk === false ||
    row.renewalDue || (row.expired && row.live) || row.status === 'unknown' || row.source !== 'indexer' || row.oldTemplate
  );
}

/** What the one line of an order says (OrderRow), from its model: the time, the price, the trigger, the filled part and the value in KAS. */
function lineFacts(row: OrderRowModel, tk: TokenInfo | undefined, b: TokenInfo | undefined, clock: Clock | null | undefined) {
  const decimals = tk?.decimals ?? RAW_TOKEN_DECIMALS;
  const genesis = row.entry.view ? BigInt(row.entry.view.genesis.daa) : null;
  const placedRecord = row.entry.record?.placedAtUnix ? BigInt(row.entry.record.placedAtUnix) : 0n;
  const unix = placedRecord > 0n ? placedRecord : genesis !== null && clock ? daaToUnix(clock, genesis) : null;
  const time = unix !== null ? formatDateTime(Number(unix), { dateStyle: 'short', timeStyle: 'short' }) : '—';
  const priceOf = (v: bigint | null): string => (v === null ? '—' : row.pair ? pairPriceText(v, row, tk, b) : formatPricePerToken(v, decimals, row.scale, { group: ',' }));
  const price = priceOf(row.price);
  const trigger = row.stopPrice !== null ? t('orders.line.stop', { price: priceOf(row.stopPrice) }) : row.armed === false && row.live ? t('orders.waitingTrigger') : '—';
  const total = row.amountTotal;
  const left = row.amountLeft;
  const filled = total !== null && left !== null ? (total > left ? total - left : 0n) : null;
  const pct = total !== null && total > 0n && filled !== null ? Number((filled * 100n) / total) : null;
  const unit = tk ? tk.ticker : row.token ? longId(row.token) : '';
  const amountText = (v: bigint) => formatUnits(v, decimals, { group: ',' });
  const filledText = total !== null && filled !== null ? `${amountText(filled)} / ${amountText(total)}` : t('orders.budgetOrder');
  const value = !row.pair && total !== null && row.price !== null ? formatKas(quoteOf(total, row.price, row.scale, 'down'), { group: ',' }) : '—';
  return { time, price, trigger, filledText, pct, unit, value };
}

const localDate = (unix: bigint): string => formatDateTime(Number(unix));

/** The fixed decimals of a price shown in every order of a market (from the token's tick): `{ fraction }`, or nothing when the market has no tick. */
const fixed = (fraction: number | null): { fraction?: number } => (fraction === null ? {} : { fraction });

/** A token the registry does not list is named by 8 + 8 hex of its covenant id (a 4 + 4 fragment can be copied by another token). */
const shortId = longId;

/**
 * A price of a pair order: B base units per `row.scale` base units of A, shown per whole A (`value x 10^decimals(A) / scale`) when A is known, in B
 * (its decimals) when B is known; raw base units of B when B is not in the registry, per `scale` base units of A when A is not.
 */
function pairPriceText(value: bigint, row: OrderRowModel, a: TokenInfo | undefined, b: TokenInfo | undefined): string {
  const aName = a?.ticker ?? (row.token ? shortId(row.token) : '?');
  // whole tokens; a token of unknown decimals on the assumed 8 (never "base units")
  const perA = row.scale > 0n ? statePriceToTokenPrice(value, a?.decimals ?? RAW_TOKEN_DECIMALS, row.scale, 'nearest') : value;
  // "X B per A", never the pair notation "A/B" (it reads as A per B)
  if (!b) return `${unitsText(perA, RAW_TOKEN_DECIMALS, ',')} ${shortId(row.pair!.quote)} per ${aName}`;
  return `${unitsText(perA, b.decimals, ',')} ${b.ticker} per ${aName}`;
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
        <Amount kind="price" value={p.value} decimals={tk.decimals} scale={p.row.scale} unit={t('orders.unit.kasPer', { ticker: tk.ticker })} {...fixed(tickPriceFraction(tk.tick, tk.decimals, p.row.scale))} group />
      ) : (
        <Amount kind="price" value={p.value} decimals={RAW_TOKEN_DECIMALS} scale={p.row.scale} unit={t('orders.unit.kasPer', { ticker: p.row.token ? shortId(p.row.token) : '?' })} group />
      )}
    </span>
  );
}

function ExpiryText({ row }: { row: OrderRowModel }) {
  const e = row.expiry;
  if (e.kind === 'day' && e.deadlineUnix !== null) {
    return <span data-testid="order-expiry">{t('orders.expiry.day', { utc: formatUtc(e.deadlineUnix), local: formatLocal(e.deadlineUnix) })}</span>;
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
  const aText = (v: bigint): string => `${unitsText(v, a?.decimals ?? RAW_TOKEN_DECIMALS, ',')} ${aName}`;
  const bText = (v: bigint): string => `${unitsText(v, b?.decimals ?? RAW_TOKEN_DECIMALS, ',')} ${bName}`;
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
  // a token the registry does not list is shown on this browser's issuance of it, else on 8 assumed decimals (app/token-display.ts)
  const bToken = row.pair ? displayToken(registry, config.network, row.pair.quote, undefined, row.pair.quoteScale) : undefined;
  const tk = token;

  const disabledReason = !canSign ? t('orders.actions.needWallet') : undefined;
  const cancelHint = row.canCancel ? undefined : row.cancelBlocked ? t(`orders.cancelBlocked.${row.cancelBlocked}`) : t('orders.actions.cannotCancel');
  // strays of a live order the wallet can spend: the maker's sweep returns them while the order lives on
  // (an order under an older contract version is only cancelled: no sweep)
  const canSweep = row.live && row.canCancel && row.strayCount > 0 && !row.cancelOnly;
  const attention = needsAttention(row);
  const [open, setOpen] = useState(attention);
  // an order that comes to need the reader later (a renewal due, an expiry, a warning after a refresh) opens its details then
  useEffect(() => {
    if (attention) setOpen(true);
  }, [attention]);
  const line = lineFacts(row, tk, bToken, props.clock);
  const marketText = tk && row.pair ? `${tk.ticker}/${bToken?.ticker ?? shortId(row.pair.quote)}` : tk ? `${tk.ticker}/KAS` : row.token ? longId(row.token) : '—';
  const cancellable = !row.oldTemplate && (row.live || row.status === 'unknown');

  return (
    <div class={`order-card or-card${props.nested ? ' nested' : ''}${open ? ' or-open' : ''}`} data-testid={`order-row-${row.id}`} data-status={row.status} data-type={row.typeKey} data-side={row.side} data-live={row.live ? '1' : '0'}>
      <div class="or-line" data-testid={`order-line-${row.id}`}>
        <span class="or-time num">{line.time}</span>
        <span class="or-market">
          {row.token ? <IdentityBlock row={{ covenantId: row.token, ticker: tk?.ticker ?? '', kascov: null }} size="sm" data-testid={`order-identity-${row.id}`} /> : null}
          <span class="or-market-name">{marketText}</span>
        </span>
        <span class="or-type">{t(`orders.type.${row.typeKey}`)}</span>
        <span class={`or-side or-side-${row.side}`}>{t(`common.side.${row.side}`)}</span>
        <span class="or-price num">{line.price}</span>
        <span class="or-trigger">{line.trigger}</span>
        <span class="or-filled">
          <span class="num">
            {line.filledText} <span class="muted small">{line.unit}</span>
            {line.pct !== null ? <span class="muted small"> {line.pct}%</span> : null}
          </span>
          {line.pct !== null ? (
            <span class="or-bar" aria-hidden="true">
              <span style={{ width: `${Math.min(100, line.pct)}%` }} />
            </span>
          ) : null}
        </span>
        <span class="or-value num">{line.value}</span>
        <span class="or-status">
          <Badge tone={STATUS_TONE[row.status]}>{t(`orders.status.${row.status}`)}</Badge>
          {attention ? (
            <span class="or-warn" title={t('orders.line.attention')} aria-label={t('orders.line.attention')} role="img">
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M12 3l9.5 17h-19z" />
                <path d="M12 10v4" />
                <path d="M12 17.5v.5" />
              </svg>
            </span>
          ) : null}
        </span>
        <span class="or-act">
          {cancellable ? (
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
          ) : null}
        </span>
        <button
          type="button"
          class="or-toggle"
          aria-expanded={open ? 'true' : 'false'}
          aria-controls={`order-details-${row.id}`}
          aria-label={t(open ? 'orders.line.hide' : 'orders.line.show')}
          onClick={() => setOpen((o) => !o)}
          data-testid={`order-toggle-${row.id}`}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M9 6l6 6-6 6" />
          </svg>
        </button>
      </div>
      <div class="or-details" id={`order-details-${row.id}`} hidden={!open} data-testid={`order-details-${row.id}`}>
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
          {row.cancelOnly ? <Badge tone="warn" title={t('orders.cancelOnly.hint')} data-testid="order-cancel-only">{t('orders.cancelOnly.badge')}</Badge> : null}
          {row.source !== 'indexer' ? <Badge tone="warn" title={t(`orders.source.${row.source}Hint`)} data-testid="order-source">{t(`orders.source.${row.source}`)}</Badge> : null}
        </div>
        <div class="small muted">
          {tk && row.pair ? (
            <a href={routeToHash(pairRoute(row.pair.base, row.pair.quote))} data-testid="order-token">{`${tk.ticker}/${bToken?.ticker ?? shortId(row.pair.quote)}`}</a>
          ) : tk ? (
            <>
              <a href={`#/market/${tk.covenantId}`} data-testid="order-token">{tokenLabel(tk)}</a>
              <TokenNote token={tk} />
            </>
          ) : row.token ? (
            <span>{longId(row.token)}</span>
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
      {row.unlisted ? (
        <Banner tone="warn" data-testid="order-unlisted">
          <p data-testid="order-unlisted-reason">{unlistedText(row.unlisted.reason)}</p>
          <p class="small" data-testid="order-unlisted-hint">{unlistedHint(row.unlisted.reason)}</p>
          <RawDetails text={row.unlisted.reason} data-testid="order-unlisted-code" />
        </Banner>
      ) : null}

      <KeyValueList
        compact
        flow
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
          // the price with the tip: only when there is a tip (else it repeats the price); an auction's is the worst too, never what was paid
          {
            label: t(row.auctionStart !== null ? 'orders.worstAllIn' : 'orders.allIn'),
            value: <PriceCell row={row} token={tk} value={row.allInPrice} />,
            'data-testid': 'order-allin',
            show: row.allInPrice !== null && row.tip > 0n,
          },
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
                    <Amount kind="token" value={row.amountLeft} decimals={tk?.decimals ?? RAW_TOKEN_DECIMALS} group />
                    {row.amountTotal !== null ? <> / <Amount kind="token" value={row.amountTotal} decimals={tk?.decimals ?? RAW_TOKEN_DECIMALS} group /></> : null}{' '}
                    {tk ? tk.ticker : row.token ? shortId(row.token) : ''}
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

      {row.oldTemplate ? (
        // nothing in this build can spend it: no Cancel button, one line why
        <p class="small muted" style="margin-top:8px" data-testid={`order-old-template-why-${row.id}`}>{t('orders.oldTemplate.noCancel')}</p>
      ) : row.live || row.status === 'unknown' ? (
        <div class="order-actions" style="margin-top:8px">
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
    </div>
  );
}
