import type { JSX } from 'preact';
import { Fragment } from 'preact';
import { useEffect } from 'preact/hooks';
import { pairRoute, routeToHash } from '../../app/router';
import { useServices } from '../../app/context';
import { useTxUrl } from '../../app/explorer';
import { formatNumber, t, RAW_TOKEN_DECIMALS } from '../../i18n';
import { unitsText } from '../../kob/pair';
import { quoteOf, statePriceToTokenPrice } from '../../kob/units';
import { formatLocal, formatUtc } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { longId, tickerUnit, type TokenInfo } from '../../kob/registry';
import { useFiat } from '../../app/use-fiat';
import {
  Amount, Badge, Banner, Button, CopyText, KeyValueList, RawDetails, type Tone,
} from '../kit';
import {
  DtCell, DtGroup, DtRow, MarketCell, MoneyCell, Num, RowToggle, TimeCell, hasOpenChoice, timeText, timeTitle, toggleLabel, useOpenRow,
} from '../kit/DataTable';
import { tokenLabel } from '../market/token-model';
import { TokenNote } from '../market/TokenNote';
import { IdentityBlock } from '../market/Fingerprint';
import type { KascovMeta } from '../market/token-model';
import { displayToken } from '../../app/token-display';
import type { EventView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { OrderFills, filledAmountOf } from './OrderFills';
import { GTC_RULE, type OrderRowModel, type OrderStatusUi } from './orders-model';
import { bidSpentByFills, filledAmount, orderEnded, orderTotalKas, placedUnix, statusText } from './orders-sort';
import { amountCell, longDate, priceDpOf, tokenUnit, type AmountText } from './cells';
import { tCount } from './plural';
import { unlistedHint, unlistedText } from './unlisted';
import { CurveDetails } from './CurveDetails';
import './order-line.css';

const STATUS_TONE: Record<OrderStatusUi, Tone> = { open: 'ok', partial: 'info', filled: 'neutral', cancelled: 'neutral', refunded: 'neutral', killed: 'neutral', closed: 'neutral', unknown: 'warn' };

/** The cells of a row in the order of its table. The keys are the columns' (orders-sort.ts). */
const OPEN_CELLS = ['time', 'market', 'type', 'side', 'price', 'amount', 'total', 'filled', 'status', 'act', 'toggle'] as const;
const HISTORY_CELLS = ['time', 'market', 'type', 'side', 'price', 'amount', 'total', 'avg', 'filled', 'status', 'toggle'] as const;

export interface OrderRowActions {
  onCancel(row: OrderRowModel): void;
  onAmend(row: OrderRowModel): void;
  onRefund(row: OrderRowModel): void;
  /** The maker's sweep of the order's strays in place. The order continues unchanged. */
  onSweep(row: OrderRowModel): void;
  /**
   * Claim, Claim and merge, and Claim and sell of what a mode-1 order bought. The order stays open.
   */
  onClaim?(row: OrderRowModel): void;
}

export interface OrderRowProps {
  row: OrderRowModel;
  token: TokenInfo | undefined;
  /** The wallet is connected on the app's network, so signing actions are enabled. */
  canSign: boolean;
  actions: OrderRowActions;
  /** An action on this order is being prepared, and the buttons show a spinner. */
  busy?: boolean;
  /** Rendered as an exit of a position, indented. */
  nested?: boolean;
  /**
   * Loads the order's events, which is the fill history on demand. Absent when there is no indexer,
   * and then there is no fill history.
   */
  loadEvents?: (id: Hex, signal: AbortSignal) => Promise<EventView[]>;
  /**
   * Wall-clock mapping of DAA scores, for the line's placement time. Without it there is no time.
   */
  clock?: Clock | null;
  /**
   * A market page's panel. The details are drawn only once opened and never open by themselves. The
   * warning mark stays on the line.
   */
  compact?: boolean;
  /**
   * kascov.io's picture of a token, for its icon. Without it the icon is a letter on the token's
   * colour.
   */
  iconOf?: (covenantId: string) => KascovMeta | null;
  /**
   * Which table the row is in, Open orders with the buttons or Order history with the average fill
   * price. The default is Open orders.
   */
  columns?: 'open' | 'history';
  /**
   * The average fill price of the order in history. Null means none and undefined means not read
   * yet.
   */
  avg?: bigint | null;
  /**
   * The fixed decimals of the table's price columns (`priceDpOf` of every row of the table), so the
   * decimal points line up. When absent, this order's own.
   */
  priceDp?: number;
  /**
   * The fixed decimals of the table's Total column in KAS. When absent, it is shown as it is, at
   * most 4.
   */
  kasDp?: number;
}

/**
 * An order that needs the reader. Its details open by themselves for a warning, a renewal, an
 * expiry or a source other than the indexer.
 */
export function needsAttention(row: OrderRowModel): boolean {
  return (
    row.unfundable || row.possiblyFrozen || !!row.unlisted || (!!row.cancelBlocked && row.cancelBlocked !== 'old-template') || row.custodyOk === false ||
    row.renewalDue || (row.expired && row.live) || row.status === 'unknown' || row.source !== 'indexer' || row.oldTemplate
  );
}

/**
 * What the one line of an order says (OrderRow), from its model. It shows the time, the price of a
 * pair order, the filled part and the value in KAS. A KAS price is drawn by its cell, with the
 * table's decimals.
 */
function lineFacts(row: OrderRowModel, tk: TokenInfo | undefined, b: TokenInfo | undefined, clock: Clock | null | undefined, avg: bigint | null | undefined) {
  const decimals = tk?.decimals ?? RAW_TOKEN_DECIMALS;
  const unix = placedUnix(row, clock);
  const time = timeText(unix);
  const pairPrice = (v: bigint | null | undefined): string => (v === null || v === undefined || !row.pair ? '—' : pairPriceText(v, row, tk, b));
  const total = row.amountTotal;
  // What the fills took, from the indexer's `filled_amount`. `amount_left` is 0 for every ended
  // order, so total - left would read a cancelled partial fill as 100 %.
  const filled = total !== null ? filledAmount(row) : null;
  const pct = total !== null && total > 0n && filled !== null ? Math.min(100, Number((filled * 100n) / total)) : null;
  // A token nobody named shows its short id in the unit slot, never the long hex (cells.ts).
  const unit = tokenUnit(tk?.ticker, row.token);
  const amountTotalText = total !== null ? amountCell(total, decimals) : null;
  const filledAmountText = filled !== null ? amountCell(filled, decimals) : null;
  // A buy order that holds a KAS budget has no amount to show. The line says so in two words, since
  // the amount follows from the price, and the details say it in full.
  const budget = !(total !== null && filled !== null);
  // A plain KAS bid shows its budget in KAS, the placed one when this browser placed it and else
  // what is left, and how much of it the fills have spent.
  const bud = row.budget;
  // What the fills spent when the placement is not in this browser: the filled tokens at their
  // average price, which Order history reads.
  const spentByFills = bidSpentByFills(row, avg);
  const budgetSpent = bud && bud.placed !== null && bud.left !== null ? bud.placed - bud.left : spentByFills;
  const budgetSpentText = budgetSpent !== null ? amountCell(budgetSpent, 8) : null;
  // The Amount column of a bid never reads "0 KAS budget". It shows the placed budget, else what is
  // left while there is some, else, for an order that is over, the KAS it spent, else just the
  // words.
  const budgetShown: { text: AmountText; unit: string; hint: string } | null =
    bud && bud.placed !== null ? { text: amountCell(bud.placed, 8), unit: t('orders.budgetUnit'), hint: t('orders.budgetPlacedHint') }
    : bud && bud.left !== null && bud.left > 0n ? { text: amountCell(bud.left, 8), unit: t('orders.budgetLeftUnit'), hint: t('orders.budgetLeftHint') }
    : bud && spentByFills !== null ? { text: amountCell(spentByFills, 8), unit: t('orders.budgetSpentUnit'), hint: t('orders.budgetSpentOnlyHint') }
    : null;
  // How much of the budget is spent. It is a share of the placed budget, else 100 % for a bid that
  // filled.
  const budgetPct = bud && budgetSpent !== null && bud.placed !== null && bud.placed > 0n ? Number((budgetSpent * 100n) / bud.placed) : bud && row.status === 'filled' ? 100 : null;
  // An open order shows its initial amount at its limit price. An ended one shows what traded,
  // which is the filled amount at the average fill price.
  const valueKas = orderTotalKas(row, avg);
  return { unix, time, timeTip: timeTitle(unix), price: pairPrice(row.price), pct, unit, valueKas, budget, budgetShown, budgetSpentText, budgetPct, amountTotalText, filledAmountText, avgText: pairPrice(avg) };
}

const localDate = longDate;

/**
 * A token the registry does not list is named by 8 + 8 hex of its covenant id, because a 4 + 4
 * fragment can be copied by another token.
 */
const shortId = longId;

/**
 * A price of a pair order, which is B base units per `row.scale` base units of A. It is shown per
 * whole A (`value x 10^decimals(A) / scale`) when A is known, in B (its decimals) when B is known,
 * as raw base units of B when B is not in the registry, and per `scale` base units of A when A is
 * not.
 */
function pairPriceText(value: bigint, row: OrderRowModel, a: TokenInfo | undefined, b: TokenInfo | undefined): string {
  const aName = a ? tickerUnit(a.ticker, a.covenantId) : row.token ? shortId(row.token) : '?';
  // Whole tokens. A token of unknown decimals is on the assumed 8, never "base units".
  const perA = row.scale > 0n ? statePriceToTokenPrice(value, a?.decimals ?? RAW_TOKEN_DECIMALS, row.scale, 'nearest') : value;
  // "X B per A", never the pair notation "A/B", which reads as A per B.
  if (!b) return `${unitsText(perA, RAW_TOKEN_DECIMALS, ',')} ${shortId(row.pair!.quote)} per ${aName}`;
  return `${unitsText(perA, b.decimals, ',')} ${tickerUnit(b.ticker, b.covenantId)} per ${aName}`;
}

/**
 * A state price of the order, in sompi per whole token of `row.scale` base units, as KAS per whole
 * token. Without the token it is per `scale` base units. A pair order's prices are in its quote
 * token B per whole A (`quote`).
 */
function PriceCell(p: { row: OrderRowModel; token: TokenInfo | undefined; quote?: TokenInfo | undefined; value: bigint | null; dp: number; testid?: string }) {
  const tk = p.token;
  const fiat = useFiat();
  if (p.value === null) return <>{'—'}</>;
  if (p.row.pair) return <span data-testid={p.testid} data-unit="pair">{pairPriceText(p.value, p.row, tk, p.quote)}</span>;
  return (
    <span data-testid={p.testid}>
      {tk ? (
        <Amount kind="price" value={p.value} decimals={tk.decimals} scale={p.row.scale} unit={t('orders.unit.kasPer', { ticker: tokenUnit(tk.ticker, p.row.token).text })} fraction={p.dp} group fiat={fiat} />
      ) : (
        <Amount kind="price" value={p.value} decimals={RAW_TOKEN_DECIMALS} scale={p.row.scale} unit={t('orders.unit.kasPer', { ticker: tokenUnit(undefined, p.row.token).text || '?' })} fraction={p.dp} group fiat={fiat} />
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

/**
 * The pair, what the order holds of each token, and the trigger rule of a pair stop. The B side
 * comes from the registry, else its short id.
 */
function pairItems(row: OrderRowModel, a: TokenInfo | undefined, b: TokenInfo | undefined) {
  const x = row.pair!;
  const aName = a ? tickerUnit(a.ticker, a.covenantId) : row.token ? shortId(row.token) : '?';
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

/**
 * One order with its type in plain words, side, all-in price, amount, status, expiry, renewal and
 * refund, custody flag, placement tx and actions.
 */
export function OrderRow(props: OrderRowProps) {
  const { row, token, canSign, actions } = props;
  const { registry, config } = useServices();
  const txUrl = useTxUrl();
  const placementUrl = txUrl(row.placementTx);
  // A token the registry does not list is shown on this browser's issuance of it, else on 8 assumed
  // decimals (app/token-display.ts).
  const bToken = row.pair ? displayToken(registry, config.network, row.pair.quote, undefined, row.pair.quoteScale) : undefined;
  const tk = token;

  const disabledReason = !canSign ? t('orders.actions.needWallet') : undefined;
  const cancelHint = row.canCancel ? undefined : row.cancelBlocked ? t(`orders.cancelBlocked.${row.cancelBlocked}`) : t('orders.actions.cannotCancel');
  // Strays of a live order the wallet can spend. The maker's sweep returns them while the order
  // lives on. An order under an older contract version is only cancelled, with no sweep.
  const canSweep = row.live && row.canCancel && row.strayCount > 0 && !row.cancelOnly;
  const attention = needsAttention(row);
  // The details a viewer opened or closed stay so across refreshes. An order that comes to need the
  // reader later opens its details then, unless the viewer has closed them. That happens for a
  // renewal due, an expiry or a warning after a refresh.
  const openKey = `order:${row.id}`;
  const [open, setOpenState] = useOpenRow(openKey, attention && !props.compact);
  const setOpen = (v: boolean) => setOpenState(v);
  useEffect(() => {
    if (attention && !props.compact && !hasOpenChoice(openKey)) setOpen(true);
  }, [attention]);
  const line = lineFacts(row, tk, bToken, props.clock, props.avg);
  // The USD display turns a KAS-per-token price of the table's cells into dollars. A pair price is
  // a price in another token and is never converted.
  const fiat = useFiat();
  // The table's price columns have the same fixed decimals in every row (`priceDp`, the finest of
  // the table), with the exact price in the cell's title. Where no table gives one, this order's
  // own is used.
  const ownDp = priceDpOf([row.price, row.stopPrice, row.allInPrice, row.auctionStart, row.auction?.currentPrice, props.avg].map((price) => ({ price, decimals: tk?.decimals ?? RAW_TOKEN_DECIMALS, scale: row.scale })));
  const dp = props.priceDp ?? ownDp;
  const priceFigure = (value: bigint | null | undefined, text: string) =>
    !row.pair && value !== null && value !== undefined ? (
      <Amount kind="price" value={value} decimals={tk?.decimals ?? RAW_TOKEN_DECIMALS} scale={row.scale} unit={false} fraction={dp} group fiat={fiat} class="dt-val" />
    ) : (
      <Num value={text} />
    );
  const marketText = tk && row.pair ? `${tickerUnit(tk.ticker, tk.covenantId)}/${bToken ? tickerUnit(bToken.ticker, bToken.covenantId) : shortId(row.pair.quote)}` : tk ? `${tickerUnit(tk.ticker, tk.covenantId)}/KAS` : row.token ? longId(row.token) : '—';
  const cancellable = !row.oldTemplate && (row.live || row.status === 'unknown');
  // What a mode-1 order bought and holds, in the bought token. A pair order's T may be B.
  const boughtTk = row.bought && row.pair && row.bought.token === row.pair.quote ? bToken : tk;
  const boughtText = row.bought ? amountCell(row.bought.amount, boughtTk?.decimals ?? RAW_TOKEN_DECIMALS).text : null;
  const boughtUnit = row.bought ? tokenUnit(boughtTk?.ticker, row.bought.token).text : '';
  const claimHint = row.bought ? t('orders.claim.hint', { amount: boughtText!, ticker: boughtUnit }) : undefined;

  // A curve's level is under its price in the table. A phone card says "Curve 4/20" on its "Sell ·
  // type" line.
  const curveLevel = row.curve ? (row.curve.priceNow === null ? t('curve.row.soldOut') : t('curve.row.level', { level: row.curve.level + 1, levels: row.curve.levels })) : null;
  const cells: Record<string, JSX.Element> = {};
  cells.time = (
    <TimeCell unix={line.unix} label={t('orders.col.time')} class="or-time" />
  );
  const marketHref = row.pair ? routeToHash(pairRoute(row.pair.base, row.pair.quote)) : row.token ? `#/market/${row.token}` : undefined;
  cells.market = (
    <MarketCell
      name={marketText}
      href={marketHref}
      identity={row.token ? <IdentityBlock scope="wallet" row={{ covenantId: row.token, ticker: tk?.ticker ?? '', kascov: props.iconOf?.(row.token) ?? null }} size="sm" data-testid={`order-identity-${row.id}`} /> : null}
      side={row.side}
      kind={row.curve && row.curve.priceNow !== null ? t('curve.row.kindShort', { level: row.curve.level + 1, levels: row.curve.levels }) : t(`orders.typeShort.${row.typeKey}`)}
      class="or-market"
      nameClass="or-market-name"
    />
  );
  cells.type = (
    <DtCell k="type" label={t('orders.col.type')} card="hidden" class="or-type" title={t(`orders.type.${row.typeKey}`)}>{t(`orders.typeShort.${row.typeKey}`)}</DtCell>
  );
  cells.side = (
    <DtCell k="side" label={t('orders.col.side')} card="hidden" class={`or-side ${row.side === 'buy' ? 'dt-side-buy' : 'dt-side-sell'}`}>{t(`common.side.${row.side}`)}</DtCell>
  );
  cells.price = (
    <DtCell k="price" label={t(fiat ? 'orders.col.priceLineUsd' : 'orders.col.priceLine')} num class="or-price">
      {priceFigure(row.price, line.price)}
      {/* a curve: one row per order, its current level under its price */}
      {curveLevel ? <div class="dt-sub or-curve-level" data-testid={`order-curve-level-${row.id}`}>{curveLevel}</div> : null}
    </DtCell>
  );
  cells.avg = (
    <DtCell k="avg" label={t(fiat ? 'orders.col.avgPriceUsd' : 'orders.col.avgPrice')} card="hidden" num class="or-avg">{priceFigure(props.avg, line.avgText)}</DtCell>
  );
  cells.amount = (
    <DtCell k="amount" label={t('orders.col.amount')} num class="or-amount" title={line.budget ? t('orders.budgetOrder') : undefined}>
      {line.amountTotalText !== null ? (
        <Num value={line.amountTotalText.text} title={line.amountTotalText.title} unit={line.unit.text} unitTitle={line.unit.title} />
      ) : line.budgetShown !== null ? (
        <Num value={line.budgetShown.text.text} title={line.budgetShown.text.title ?? line.budgetShown.hint} unit={line.budgetShown.unit} />
      ) : (
        <span class="dt-val">{t('orders.budgetShort')}</span>
      )}
    </DtCell>
  );
  cells.filled = (
    <DtCell k="filled" label={t('orders.col.filled')} card="meter" num class="or-filled">
      <span class="num">
        {line.filledAmountText !== null ? (
          <Num value={line.filledAmountText.text} title={line.filledAmountText.title} unit={line.unit.text} unitTitle={line.unit.title} />
        ) : line.budgetSpentText !== null ? (
          <Num value={line.budgetSpentText.text} title={line.budgetSpentText.title ?? t('orders.budgetSpentHint')} unit="KAS" />
        ) : (
          <span class="dt-val">—</span>
        )}
        {(line.pct ?? line.budgetPct) !== null ? <span class="dt-unit or-pct"> {line.pct ?? line.budgetPct}%</span> : null}
      </span>
      {(line.pct ?? line.budgetPct) !== null ? (
        <span class="or-bar" aria-hidden="true">
          <span style={{ width: `${Math.min(100, line.pct ?? line.budgetPct ?? 0)}%` }} />
        </span>
      ) : null}
      {boughtText !== null && row.live ? (
        <div class="dt-sub" title={t('orders.bought.inOrderHint')} data-testid={`order-bought-${row.id}`}>{t('orders.bought.inOrder', { amount: boughtText })}</div>
      ) : null}
    </DtCell>
  );
  cells.total = (
    <DtCell k="total" label={t(props.columns === 'history' ? (fiat ? 'orders.col.tradedUsd' : 'orders.col.traded') : fiat ? 'orders.col.totalUsd' : 'orders.col.total')} num class="or-value" title={t(orderEnded(row) ? 'orders.total.tradedTip' : 'orders.total.limitTip')}><MoneyCell kas={line.valueKas} unit={false} {...(props.kasDp !== undefined ? { fraction: props.kasDp } : {})} /></DtCell>
  );
  cells.status = (
    <DtCell k="status" label={t('orders.col.status')} card="status" class="or-status">
      <Badge tone={STATUS_TONE[row.status]}>{statusText(row)}</Badge>
      {attention ? (
        <span class="or-warn" title={t('orders.line.attention')} aria-label={t('orders.line.attention')} role="img">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 3l9.5 17h-19z" />
            <path d="M12 10v4" />
            <path d="M12 17.5v.5" />
          </svg>
        </span>
      ) : null}
    </DtCell>
  );
  cells.act = (
    <DtCell k="act" card="action" class="or-act">
      {cancellable ? (
        <span class="dt-act">
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
        </span>
      ) : null}
    </DtCell>
  );
  cells.toggle = <RowToggle open={open} onToggle={() => setOpen(!open)} controls={`order-details-${row.id}`} label={toggleLabel(open)} data-testid={`order-toggle-${row.id}`} />;

  return (
    <DtGroup class={`or-group${props.nested ? ' nested' : ''}${open ? ' or-open' : ''}`} data-testid={`order-row-${row.id}`} data-status={row.status} data-type={row.typeKey} data-side={row.side} data-live={row.live ? '1' : '0'}>
      <DtRow class="or-line" data-testid={`order-line-${row.id}`}>
        {(props.columns === 'history' ? HISTORY_CELLS : OPEN_CELLS).map((k) => (
          <Fragment key={k}>{cells[k]}</Fragment>
        ))}
      </DtRow>
      {props.compact && !open ? null : (
      <div class="dt-details or-details" id={`order-details-${row.id}`} hidden={!open} data-testid={`order-details-${row.id}`}>
      <div class="row-between">
        <div class="row" style="gap:6px">
          <strong data-testid={`order-type-${row.id}`}>{t(`orders.type.${row.typeKey}`)}</strong>
          {placementUrl ? (
            <a class="explorer-link small" href={placementUrl} target="_blank" rel="noopener noreferrer" title={t('orders.explorerHint')} aria-label={t('orders.explorerHint')} data-testid={`order-explorer-${row.id}`}>
              {t('orders.explorer')}
            </a>
          ) : null}
          <Badge tone={row.side === 'buy' ? 'ok' : 'bad'} data-testid="order-side">{t(`common.side.${row.side}`)}</Badge>
          <Badge tone={STATUS_TONE[row.status]} data-testid="order-status">{statusText(row)}</Badge>
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
            <a href={routeToHash(pairRoute(row.pair.base, row.pair.quote))} data-testid="order-token">{`${tickerUnit(tk.ticker, tk.covenantId)}/${bToken ? tickerUnit(bToken.ticker, bToken.covenantId) : shortId(row.pair.quote)}`}</a>
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

      {row.curve ? <CurveDetails id={row.id} curve={row.curve} amountLeft={row.amountLeft} scale={row.scale} decimals={tk?.decimals ?? RAW_TOKEN_DECIMALS} /> : null}

      <KeyValueList
        compact
        flow
        items={[
          ...(row.pair ? pairItems(row, tk, bToken) : []),
          // An auction's bound is its END price, the worst it can fill at, and is never labelled a
          // limit. Its start is only the expected price.
          {
            label: t(row.curve ? 'curve.details.now' : row.auctionStart !== null ? 'orders.worstPrice' : 'orders.limitPrice'),
            value: <PriceCell row={row} token={tk} quote={bToken} value={row.price} dp={ownDp} />,
            'data-testid': 'order-price',
            show: row.price !== null || row.stopPrice === null,
          },
          { label: t('orders.startPrice'), value: <PriceCell row={row} token={tk} quote={bToken} value={row.auctionStart} dp={ownDp} />, 'data-testid': 'order-start-price', show: row.auctionStart !== null },
          { label: t('orders.stopPrice'), value: <PriceCell row={row} token={tk} quote={bToken} value={row.stopPrice} dp={ownDp} />, 'data-testid': 'order-stop', show: row.stopPrice !== null },
          // The price with the tip is shown only when there is a tip, as it would otherwise repeat
          // the price. An auction's is the worst too, never what was paid.
          {
            label: t(row.auctionStart !== null ? 'orders.worstAllIn' : 'orders.allIn'),
            value: <PriceCell row={row} token={tk} value={row.allInPrice} dp={ownDp} />,
            'data-testid': 'order-allin',
            show: row.allInPrice !== null && row.tip > 0n,
          },
          {
            label: t('orders.auctionNow'),
            value: <PriceCell row={row} token={tk} value={row.auction?.currentPrice ?? null} dp={ownDp} />,
            show: row.auction !== null && !row.auction.complete && row.live && !row.pair,
          },
          {
            label: t('orders.amount'),
            value: (
              <span data-testid="order-amount">
                {row.amountLeft !== null ? (
                  <>
                    {row.amountEstimated ? <span class="est" title={t('market.book.estimatedNote')}>~</span> : null}
                    <Amount kind="token" value={row.amountLeft} decimals={tk?.decimals ?? RAW_TOKEN_DECIMALS} maxFraction={4} group />
                    {row.amountTotal !== null ? <> / <Amount kind="token" value={row.amountTotal} decimals={tk?.decimals ?? RAW_TOKEN_DECIMALS} maxFraction={4} group /></> : null}{' '}
                    <span title={line.unit.title}>{line.unit.text}</span>
                  </>
                ) : (
                  <>{t('orders.budgetOrder')}</>
                )}
              </span>
            ),
          },
          {
            label: t('orders.budgetDetail'),
            value: (
              <span data-testid="order-budget">
                {row.budget && row.budget.left !== null
                  ? row.budget.placed !== null
                    ? t('orders.budgetOf', { left: amountCell(row.budget.left, 8).text, placed: amountCell(row.budget.placed, 8).text })
                    : t('orders.budgetLeftOnly', { left: amountCell(row.budget.left, 8).text })
                  : ''}
              </span>
            ),
            show: !!row.budget && row.budget.left !== null,
          },
          // What a phone's card leaves out of its lines: the average fill price (Order history) and
          // the filled part with its unit.
          { label: t(fiat ? 'orders.col.avgPriceUsd' : 'orders.col.avgPrice'), value: <span data-testid="order-avg">{priceFigure(props.avg, line.avgText)}</span>, show: props.columns === 'history' },
          {
            label: t('orders.col.filled'),
            value: (
              <span data-testid="order-filled">
                {line.filledAmountText !== null ? `${line.filledAmountText.text} ${line.unit.text}` : line.budgetSpentText !== null ? `${line.budgetSpentText.text} KAS` : '—'}
                {(line.pct ?? line.budgetPct) !== null ? ` (${line.pct ?? line.budgetPct}%)` : ''}
              </span>
            ),
            show: line.filledAmountText !== null || line.budgetSpentText !== null,
          },
          {
            label: t('orders.detail.delivery'),
            value: <span data-testid="order-delivery">{t(`ticket.short.delivery.${row.delivery === '1' ? 'collected' : row.delivery === '2' ? 'topup' : 'each'}`)}</span>,
            show: row.delivery !== null,
          },
          {
            label: t('orders.detail.bought'),
            value: <span data-testid="order-bought-detail">{t('orders.detail.boughtValue', { amount: boughtText ?? '', ticker: boughtUnit })}</span>,
            show: boughtText !== null && row.live,
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
            value: <span data-testid="order-strays">{tCount(canSweep ? 'orders.straysValueSweep' : 'orders.straysValue', row.strayCount)}</span>,
            show: row.strayCount > 0,
          },
          { label: t('orders.note'), value: row.label ?? '', show: !!row.label },
          {
            label: t('orders.placementTx'),
            value: row.placementTx ? <CopyText value={row.placementTx} ellipsis href={placementUrl} data-testid={`order-tx-${row.id}`} /> : '—',
          },
          { label: t('orders.orderId'), value: <CopyText value={row.id} chars={14} data-testid={`order-id-${row.id}`} /> },
        ]}
      />

      {props.loadEvents && filledAmountOf(row) > 0n ? <OrderFills row={row} token={tk} quote={bToken} network={config.network} loadEvents={props.loadEvents} /> : null}

      {row.oldTemplate ? (
        // Nothing in this build can spend it, so there is no Cancel button and one line says why.
        <p class="small muted" style="margin-top:8px" data-testid={`order-old-template-why-${row.id}`}>{t('orders.oldTemplate.noCancel')}</p>
      ) : row.live || row.status === 'unknown' ? (
        <div class="order-actions" style="margin-top:8px">
          {/* in the details, not the line: the line's action cell keeps its width (founder rule, the Portfolio layout) */}
          {row.canClaim && actions.onClaim ? (
            <Button small variant="primary" disabled={!canSign || props.busy} title={disabledReason ?? claimHint} onClick={() => actions.onClaim!(row)} data-testid={`order-claim-${row.id}`}>
              {t('orders.claim')}
            </Button>
          ) : null}
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
      )}
    </DtGroup>
  );
}
