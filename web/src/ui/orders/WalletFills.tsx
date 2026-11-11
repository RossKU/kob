import { useMemo } from 'preact/hooks';
import type { JSX } from 'preact';
import type { EventView } from '../../data/indexer-types';
import { useTxUrl } from '../../app/explorer';
import { useFiat } from '../../app/use-fiat';
import { pairRoute, routeToHash } from '../../app/router';
import { t, RAW_TOKEN_DECIMALS } from '../../i18n';
import { longId, type TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { quoteOf } from '../../kob/units';
import {
  Amount, CopyText, EmptyState, ErrorBanner, KeyValueList, Loading, columnFraction, scaledUnits, sortBy, useAsync, type SortState, type SortValue,
} from '../kit';
import {
  DataTable, DtCell, DtGroup, DtRow, KAS_FRACTION_MAX, MarketCell, MoneyCell, Num, RowToggle, TimeCell, timeText, toggleLabel, useOpenRow, type DtColumn, type DtPlain,
} from '../kit/DataTable';
import { IdentityBlock } from '../market/Fingerprint';
import type { KascovMeta } from '../market/token-model';
import { fillHistory, type FillRow } from './fills-model';
import { PortfolioPager, pageOf } from './portfolio-paging';
import type { OrderRowModel } from './orders-model';
import { amountCell, priceDpOf, tokenUnit, type AmountText, type UnitText } from './cells';

export const FILL_SORT_KEYS = ['time', 'market', 'side', 'price', 'amount', 'value', 'tx'] as const;
export type FillSortKey = (typeof FILL_SORT_KEYS)[number];

/** The columns of the fills table, in its order; `time` is the table's own order (newest first). */
export function fillColumns(usd = false): (DtColumn<FillSortKey> | DtPlain)[] {
  return [
    { key: 'time', label: t('orders.fills.time'), kind: 'time', card: 'badge' },
    { key: 'market', label: t('orders.col.market'), kind: 'text', card: 'title' },
    { key: 'side', label: t('orders.col.side'), kind: 'text', card: 'hidden' },
    { key: 'price', label: t(usd ? 'orders.col.priceUsd' : 'orders.col.price'), kind: 'number', right: true },
    { key: 'amount', label: t('orders.col.amount'), kind: 'number', right: true },
    { key: 'value', label: t(usd ? 'orders.col.valueUsd' : 'orders.col.valueKas'), kind: 'number', right: true },
    { key: 'tx', label: t('orders.fills.tx'), kind: 'text', card: 'hidden' },
    { key: 'toggle', plain: true },
  ];
}

/** Events of a few orders at a time: one request per order that filled. */
const BATCH = 4;

/**
 * The fills last read, per list of orders (their ids and statuses), kept for the visit: the tab is drawn anew each time it is opened, and the rows of the last visit stand
 * at once while the read is made again (as the Open orders and Order history lists, which live in the page, do), instead of a spinner. A few lists are kept: the
 * Portfolio's and a market page's panel have their own.
 */
const fillsSeen = new Map<string, WalletFill[]>();
const FILLS_SEEN_MAX = 8;
const rememberFills = (key: string, fills: WalletFill[]): void => {
  fillsSeen.delete(key);
  fillsSeen.set(key, fills);
  if (fillsSeen.size > FILLS_SEEN_MAX) fillsSeen.delete(fillsSeen.keys().next().value!);
};

interface WalletFill {
  fill: FillRow;
  row: OrderRowModel;
  token: TokenInfo | undefined;
}

interface Facts {
  unit: UnitText;
  market: string;
  marketHref: string | undefined;
  valueKas: bigint | null;
  unix: number | null;
  time: string;
  amount: AmountText;
  identity: JSX.Element | null;
  key: string;
}

/**
 * The Portfolio page's Fills tab: every fill of the wallet's orders, newest first, one row each (market, side, price, amount, value, time, the transaction), in
 * the shared table (kit/DataTable.tsx). Read when the tab opens: the events of each order that filled (the same requests as the CSV export of every fill).
 */
export function WalletFills(props: {
  rows: readonly OrderRowModel[];
  tokenOf(id: string | null, scale?: bigint | null): TokenInfo | undefined;
  loadEvents: ((id: Hex, signal: AbortSignal) => Promise<EventView[]>) | null;
  /** kascov.io's picture of a token, for its icon */
  iconOf?: (covenantId: string) => KascovMeta | null;
  /** the viewer's sort of the fills (none: newest first) and how it is changed */
  sort: SortState<FillSortKey> | null;
  onSort: (s: SortState<FillSortKey> | null) => void;
  /** the rows of a page and the page shown (0 first): the newest fills come first */
  size: number;
  page: number;
  onPage: (page: number) => void;
}) {
  const txUrl = useTxUrl();
  // the USD display: the price and the value of a fill in dollars; the amount of tokens never
  const fiat = useFiat();
  const key = props.rows.map((r) => `${r.id}:${r.status}`).join(',');
  const data = useAsync(
    async (signal) => {
      if (!props.loadEvents) return null;
      const out: WalletFill[] = [];
      for (let i = 0; i < props.rows.length; i += BATCH) {
        const part = props.rows.slice(i, i + BATCH);
        const lists = await Promise.all(part.map((r) => props.loadEvents!(r.id, signal)));
        part.forEach((row, k) => {
          const token = props.tokenOf(row.token, row.scale);
          for (const fill of fillHistory(lists[k]!, row.scale).rows) out.push({ fill, row, token });
        });
      }
      out.sort((a, b) => b.fill.atMs - a.fill.atMs);
      if (!signal.aborted) rememberFills(key, out);
      return out;
    },
    [key, !!props.loadEvents],
  );
  const quoteOfPair = useMemo(() => new Map<string, TokenInfo | undefined>(), []);
  const marketOf = ({ row, token }: WalletFill): string => {
    const ticker = token?.ticker ?? (row.token ? longId(row.token) : '?');
    let quote: TokenInfo | undefined;
    if (row.pair) {
      if (!quoteOfPair.has(row.pair.quote)) quoteOfPair.set(row.pair.quote, props.tokenOf(row.pair.quote, row.pair.quoteScale));
      quote = quoteOfPair.get(row.pair.quote);
    }
    return row.pair ? `${ticker}/${quote?.ticker ?? longId(row.pair.quote)}` : `${ticker}/KAS`;
  };
  /** What a column compares: the amounts as tokens (the tokens have their own decimals), the KAS value as sompi, the time as milliseconds. */
  const valueOf = (w: WalletFill, key: FillSortKey): SortValue => {
    const { fill, row, token } = w;
    switch (key) {
      case 'time':
        return fill.atMs > 0 ? fill.atMs : null;
      case 'market':
        return marketOf(w);
      case 'side':
        return t(`common.side.${row.side}`);
      case 'price':
        return !row.pair && fill.price !== null ? fill.price : null;
      case 'amount':
        return scaledUnits(fill.amount, token?.decimals ?? RAW_TOKEN_DECIMALS);
      case 'value':
        return !row.pair && fill.price !== null ? quoteOf(fill.amount, fill.price, row.scale, 'down') : null;
      case 'tx':
        return fill.txid;
    }
  };
  // what is read now, else what the last visit read for this very list
  const read = data.data ?? fillsSeen.get(key);
  const all = read ? sortBy(read, props.sort, valueOf) : read;
  const { rows: list, window: win } = pageOf(all ?? [], props.size, props.page);
  // the price column has one number of decimals (the finest of the whole list, so a page turned keeps it), the KAS value column too (at most 4); the exact figures are the titles
  const priceDp = priceDpOf((all ?? []).filter((w) => !w.row.pair).map((w) => ({ price: w.fill.price, decimals: w.token?.decimals ?? RAW_TOKEN_DECIMALS, scale: w.row.scale })));
  const kasDp = Math.min(KAS_FRACTION_MAX, columnFraction((all ?? []).map((w) => (!w.row.pair && w.fill.price !== null ? quoteOf(w.fill.amount, w.fill.price, w.row.scale, 'down') : null)), 8));
  const cols = fillColumns(!!fiat);
  const ownCol = cols.find((c) => c.key === 'time') as DtColumn<FillSortKey>;
  const pager = all ? <PortfolioPager id="wallet-fills" window={win} total={all.length} onPage={props.onPage} /> : null;

  /** What one fill says. */
  const factsOf = (w: WalletFill): Facts => {
    const { fill, row, token } = w;
    const decimals = token?.decimals ?? RAW_TOKEN_DECIMALS;
    const unit = tokenUnit(token?.ticker, row.token);
    const marketHref = row.pair ? routeToHash(pairRoute(row.pair.base, row.pair.quote)) : row.token ? `#/market/${row.token}` : undefined;
    // a KAS market's fill has a price and a KAS value (drawn by their cells with the table's decimals); a pair fill is volume only (the B amount stands for its value)
    const valueKas = !row.pair && fill.price !== null ? quoteOf(fill.amount, fill.price, row.scale, 'down') : null;
    const unix = fill.atMs > 0 ? Math.floor(fill.atMs / 1000) : null;
    const time = timeText(unix);
    const amount = amountCell(fill.amount, decimals);
    const identity = row.token ? <IdentityBlock row={{ covenantId: row.token, ticker: token?.ticker ?? '', kascov: props.iconOf?.(row.token) ?? null }} size="sm" /> : null;
    return { unit, market: marketOf(w), marketHref, valueKas, unix, time, amount, identity, key: `${fill.txid}-${row.id}-${fill.atMs}` };
  };

  if (!props.loadEvents) return <p class="muted" data-testid="wallet-fills-off">{t('orders.fills.noIndexer')}</p>;
  return (
    <div class="stack-sm" data-testid="wallet-fills">
      {data.loading && !all ? <Loading /> : null}
      <ErrorBanner error={data.error} onRetry={data.reload} />
      {all ? (
        <DataTable id="wallet-fills" grid="fills" label={t('orders.fills.title')} cols={cols} sort={props.sort} own={ownCol} onSort={props.onSort} sortable={all.length > 1} body={{ class: 'ml-in' }} data-testid="wallet-fills-table">
          {list.length === 0 ? <EmptyState icon="list" compact data-testid="wallet-fills-empty">{t('orders.fills.none')}</EmptyState> : null}
          {list.map((item) => {
            const f = factsOf(item);
            return <FillRowView key={f.key} item={item} f={f} txUrl={txUrl} fiat={!!fiat} priceDp={priceDp} kasDp={kasDp} />;
          })}
        </DataTable>
      ) : null}
      {pager}
    </div>
  );
}

/** One fill: its row, and under it (on a chevron) the order and the transaction in full. */
function FillRowView(props: { item: WalletFill; f: Facts; txUrl: (txid: string) => string | null; fiat: boolean; priceDp: number; kasDp: number }) {
  const fiat = useFiat();
  const { fill, row } = props.item;
  const f = props.f;
  const [open, setOpen] = useOpenRow(`fills:${f.key}`, false);
  const href = props.txUrl(fill.txid);
  const detailsId = `fill-details-${f.key}`;
  return (
    <DtGroup data-testid="wallet-fill-row" data-txid={fill.txid} data-order={row.id}>
      <DtRow>
        <TimeCell unix={f.unix} label={t('orders.fills.time')} />
        <MarketCell name={f.market} href={f.marketHref} identity={f.identity} side={row.side} />
        <DtCell k="side" label={t('orders.col.side')} card="hidden" data-testid="wallet-fill-side" class={row.side === 'buy' ? 'dt-side-buy' : 'dt-side-sell'}>{t(`common.side.${row.side}`)}</DtCell>
        <DtCell k="price" label={t(props.fiat ? 'orders.col.priceUsd' : 'orders.col.price')} num>
          {!row.pair && fill.price !== null ? <Amount kind="price" value={fill.price} decimals={props.item.token?.decimals ?? RAW_TOKEN_DECIMALS} scale={row.scale} unit={false} fraction={props.priceDp} group fiat={fiat} class="dt-val" /> : <Num value="—" />}
        </DtCell>
        <DtCell k="amount" label={t('orders.col.amount')} num><Num value={f.amount.text} title={f.amount.title} unit={f.unit.text} unitTitle={f.unit.title} /></DtCell>
        <DtCell k="value" label={t(props.fiat ? 'orders.col.valueUsd' : 'orders.col.valueKas')} num><MoneyCell kas={f.valueKas} unit={false} fraction={props.kasDp} /></DtCell>
        <DtCell k="tx" label={t('orders.fills.tx')} card="hidden"><CopyText value={fill.txid} ellipsis href={href} data-testid="wallet-fill-tx" /></DtCell>
        <RowToggle open={open} onToggle={() => setOpen(!open)} controls={detailsId} label={toggleLabel(open)} data-testid="wallet-fill-toggle" />
      </DtRow>
      <div class="dt-details" id={detailsId} hidden={!open} data-testid="wallet-fill-details">
        <KeyValueList
          compact
          flow
          items={[
            { label: t('orders.orderId'), value: <CopyText value={row.id} head={8} tail={6} /> },
            { label: t('orders.fills.tx'), value: <CopyText value={fill.txid} ellipsis href={href} /> },
            { label: t('orders.fills.time'), value: f.time },
            { label: t(props.fiat ? 'orders.col.valueUsd' : 'orders.col.valueKas'), value: <MoneyCell kas={f.valueKas} /> },
          ]}
        />
      </div>
    </DtGroup>
  );
}
