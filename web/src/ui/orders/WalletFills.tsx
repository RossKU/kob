import { useMemo } from 'preact/hooks';
import type { JSX } from 'preact';
import type { EventView } from '../../data/indexer-types';
import { useTxUrl } from '../../app/explorer';
import { pairRoute, routeToHash } from '../../app/router';
import { formatDateTime, t, RAW_TOKEN_DECIMALS } from '../../i18n';
import { longId, type TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { formatUnits, quoteOf } from '../../kob/units';
import {
  CopyText, DataTable, DtCell, DtGroup, DtRow, ErrorBanner, KeyValueList, Loading, MoneyCell, Num, RowToggle, fixedPricePerToken, scaledUnits, sortBy, tickPriceFraction, toggleLabel, useAsync, useOpenRow,
  type DtColumn, type DtPlain, type SortState, type SortValue,
} from '../kit';
import { IdentityBlock } from '../market/Fingerprint';
import type { KascovMeta } from '../market/token-model';
import { fillHistory, type FillRow } from './fills-model';
import { PortfolioPager, pageOf } from './portfolio-paging';
import type { OrderRowModel } from './orders-model';

export const FILL_SORT_KEYS = ['time', 'market', 'side', 'price', 'amount', 'value', 'tx'] as const;
export type FillSortKey = (typeof FILL_SORT_KEYS)[number];

/** The columns of the fills table, in its order; `time` is the table's own order (newest first). */
export function fillColumns(): (DtColumn<FillSortKey> | DtPlain)[] {
  return [
    { key: 'time', label: t('orders.fills.time'), kind: 'time', card: 'badge' },
    { key: 'market', label: t('orders.col.market'), kind: 'text', card: 'title' },
    { key: 'side', label: t('orders.col.side'), kind: 'text', card: 'hidden' },
    { key: 'price', label: t('orders.col.price'), kind: 'number', right: true },
    { key: 'amount', label: t('orders.col.amount'), kind: 'number', right: true },
    { key: 'value', label: t('orders.col.valueKas'), kind: 'number', right: true },
    { key: 'tx', label: t('orders.fills.tx'), kind: 'text' },
    { key: 'toggle', plain: true },
  ];
}

/** Events of a few orders at a time: one request per order that filled. */
const BATCH = 4;

interface WalletFill {
  fill: FillRow;
  row: OrderRowModel;
  token: TokenInfo | undefined;
}

interface Facts {
  ticker: string;
  market: string;
  marketHref: string | undefined;
  price: string;
  valueKas: bigint | null;
  time: string;
  amount: string;
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
      return out.sort((a, b) => b.fill.atMs - a.fill.atMs);
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
  const all = data.data ? sortBy(data.data, props.sort, valueOf) : data.data;
  const { rows: list, window: win } = pageOf(all ?? [], props.size, props.page);
  const cols = fillColumns();
  const ownCol = cols.find((c) => c.key === 'time') as DtColumn<FillSortKey>;
  const pager = all ? <PortfolioPager id="wallet-fills" window={win} total={all.length} onPage={props.onPage} /> : null;

  /** What one fill says. */
  const factsOf = (w: WalletFill): Facts => {
    const { fill, row, token } = w;
    const decimals = token?.decimals ?? RAW_TOKEN_DECIMALS;
    const ticker = token?.ticker ?? (row.token ? longId(row.token) : '?');
    const marketHref = row.pair ? routeToHash(pairRoute(row.pair.base, row.pair.quote)) : row.token ? `#/market/${row.token}` : undefined;
    // a KAS market's fill has a price and a KAS value; a pair fill is volume only (the B amount stands for its value)
    const price = !row.pair && fill.price !== null ? fixedPricePerToken(fill.price, decimals, row.scale, (token ? tickPriceFraction(token.tick, decimals, row.scale) : null) ?? 8) : '—';
    const valueKas = !row.pair && fill.price !== null ? quoteOf(fill.amount, fill.price, row.scale, 'down') : null;
    const time = fill.atMs > 0 ? formatDateTime(Math.floor(fill.atMs / 1000), { dateStyle: 'short', timeStyle: 'medium' }) : '—';
    const amount = formatUnits(fill.amount, decimals, { group: ',' });
    const identity = row.token ? <IdentityBlock row={{ covenantId: row.token, ticker: token?.ticker ?? '', kascov: props.iconOf?.(row.token) ?? null }} size="sm" /> : null;
    return { ticker, market: marketOf(w), marketHref, price, valueKas, time, amount, identity, key: `${fill.txid}-${row.id}-${fill.atMs}` };
  };

  if (!props.loadEvents) return <p class="muted" data-testid="wallet-fills-off">{t('orders.fills.noIndexer')}</p>;
  return (
    <div class="stack-sm" data-testid="wallet-fills">
      {data.loading && !all ? <Loading /> : null}
      <ErrorBanner error={data.error} onRetry={data.reload} />
      {all ? (
        <DataTable id="wallet-fills" grid="fills" label={t('orders.fills.title')} cols={cols} sort={props.sort} own={ownCol} onSort={props.onSort} sortable={all.length > 1} data-testid="wallet-fills-table">
          {list.length === 0 ? <p class="dt-empty" data-testid="wallet-fills-empty">{t('orders.fills.none')}</p> : null}
          {list.map((item) => {
            const f = factsOf(item);
            return <FillRowView key={f.key} item={item} f={f} txUrl={txUrl} />;
          })}
        </DataTable>
      ) : null}
      {pager}
    </div>
  );
}

/** One fill: its row, and under it (on a chevron) the order and the transaction in full. */
function FillRowView(props: { item: WalletFill; f: Facts; txUrl: (txid: string) => string | null }) {
  const { fill, row } = props.item;
  const f = props.f;
  const [open, setOpen] = useOpenRow(`fills:${f.key}`, false);
  const href = props.txUrl(fill.txid);
  const detailsId = `fill-details-${f.key}`;
  return (
    <DtGroup data-testid="wallet-fill-row" data-txid={fill.txid} data-order={row.id}>
      <DtRow>
        <DtCell k="time" label={t('orders.fills.time')} card="badge" class="dt-time">{f.time}</DtCell>
        <DtCell k="market" card="title">
          <span class="dt-market">
            {f.identity}
            <span class="dt-market-name">{f.marketHref ? <a href={f.marketHref}>{f.market}</a> : f.market}</span>
          </span>
          <span class="dt-card-only dt-sub"><span class={row.side === 'buy' ? 'dt-side-buy' : 'dt-side-sell'}>{t(`common.side.${row.side}`)}</span></span>
        </DtCell>
        <DtCell k="side" label={t('orders.col.side')} card="hidden" data-testid="wallet-fill-side" class={row.side === 'buy' ? 'dt-side-buy' : 'dt-side-sell'}>{t(`common.side.${row.side}`)}</DtCell>
        <DtCell k="price" label={t('orders.col.price')} num><Num value={f.price} /></DtCell>
        <DtCell k="amount" label={t('orders.col.amount')} num><Num value={f.amount} unit={f.ticker} /></DtCell>
        <DtCell k="value" label={t('orders.col.valueKas')} num><MoneyCell kas={f.valueKas} unit={false} /></DtCell>
        <DtCell k="tx" label={t('orders.fills.tx')} card="wide"><CopyText value={fill.txid} head={6} tail={4} href={href} data-testid="wallet-fill-tx" /></DtCell>
        <RowToggle open={open} onToggle={() => setOpen(!open)} controls={detailsId} label={toggleLabel(open)} data-testid="wallet-fill-toggle" />
      </DtRow>
      <div class="dt-details" id={detailsId} hidden={!open} data-testid="wallet-fill-details">
        <KeyValueList
          compact
          flow
          items={[
            { label: t('orders.orderId'), value: <CopyText value={row.id} head={8} tail={6} /> },
            { label: t('orders.fills.tx'), value: <CopyText value={fill.txid} short={false} href={href} /> },
            { label: t('orders.fills.time'), value: f.time },
            { label: t('orders.col.valueKas'), value: <MoneyCell kas={f.valueKas} /> },
          ]}
        />
      </div>
    </DtGroup>
  );
}
