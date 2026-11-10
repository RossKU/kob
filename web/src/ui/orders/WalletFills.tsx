import { useMemo } from 'preact/hooks';
import type { EventView } from '../../data/indexer-types';
import { useTxUrl } from '../../app/explorer';
import { pairRoute, routeToHash } from '../../app/router';
import { formatDateTime, t, RAW_TOKEN_DECIMALS } from '../../i18n';
import { longId, type TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { formatKas, formatUnits, quoteOf } from '../../kob/units';
import { CopyText, ErrorBanner, SortButton, SortControl, ariaSortOf, fixedPricePerToken, Loading, Table, TableMessage, scaledUnits, sortBy, tickPriceFraction, useAsync, type SortColumn, type SortState, type SortValue } from '../kit';
import { IdentityBlock } from '../market/Fingerprint';
import type { KascovMeta } from '../market/token-model';
import { fillHistory, type FillRow } from './fills-model';
import { PortfolioPager, pageOf } from './portfolio-paging';
import { TABLE_FROM, useTableCards } from './use-table-cards';
import './balances.css';
import type { OrderRowModel } from './orders-model';

export const FILL_SORT_KEYS = ['time', 'market', 'side', 'price', 'amount', 'value', 'tx'] as const;
export type FillSortKey = (typeof FILL_SORT_KEYS)[number];

/** The columns of the fills table, in its order; `time` is the list's own order (newest first). */
export function fillColumns(): SortColumn<FillSortKey>[] {
  return [
    { key: 'time', label: t('orders.fills.time'), kind: 'time' },
    { key: 'market', label: t('orders.col.market'), kind: 'text' },
    { key: 'side', label: t('orders.col.side'), kind: 'text' },
    { key: 'price', label: t('orders.col.price'), kind: 'number' },
    { key: 'amount', label: t('orders.col.amount'), kind: 'number' },
    { key: 'value', label: t('orders.col.valueKas'), kind: 'number' },
    { key: 'tx', label: t('orders.fills.tx'), kind: 'text' },
  ];
}

/** Events of a few orders at a time: one request per order that filled. */
const BATCH = 4;

interface WalletFill {
  fill: FillRow;
  row: OrderRowModel;
  token: TokenInfo | undefined;
}

/**
 * The Portfolio page's Fills tab: every fill of the wallet's orders, newest first, one line each (time, market, side, price, amount, value, the
 * transaction). Read when the tab opens: the events of each order that filled (the same requests as the CSV export of every fill).
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
  const ownCol = cols[0]!;
  const pager = all ? <PortfolioPager id="wallet-fills" window={win} total={all.length} onPage={props.onPage} /> : null;
  // a narrow box reads each fill as a card (market, side and price; amount and value; time and transaction), a wide one as a table row (use-table-cards.ts)
  const [box, phone] = useTableCards(TABLE_FROM);

  /** What one fill says, in the words of both layouts. */
  const factsOf = ({ fill, row, token }: WalletFill) => {
    const decimals = token?.decimals ?? RAW_TOKEN_DECIMALS;
    const ticker = token?.ticker ?? (row.token ? longId(row.token) : '?');
    const market = marketOf({ fill, row, token });
    const marketHref = row.pair ? routeToHash(pairRoute(row.pair.base, row.pair.quote)) : row.token ? `#/market/${row.token}` : undefined;
    // a KAS market's fill has a price and a KAS value; a pair fill is volume only (the B amount stands for its value)
    const price = !row.pair && fill.price !== null ? fixedPricePerToken(fill.price, decimals, row.scale, (token ? tickPriceFraction(token.tick, decimals, row.scale) : null) ?? 8) : '—';
    const value = !row.pair && fill.price !== null ? formatKas(quoteOf(fill.amount, fill.price, row.scale, 'down'), { group: ',' }) : '—';
    const time = fill.atMs > 0 ? formatDateTime(Math.floor(fill.atMs / 1000), { dateStyle: 'short', timeStyle: 'medium' }) : '—';
    const amount = formatUnits(fill.amount, decimals, { group: ',' });
    const identity = row.token ? <IdentityBlock row={{ covenantId: row.token, ticker: token?.ticker ?? '', kascov: props.iconOf?.(row.token) ?? null }} size="sm" /> : null;
    return { ticker, market, marketHref, price, value, time, amount, identity, key: `${fill.txid}-${row.id}-${fill.atMs}` };
  };

  if (!props.loadEvents) return <p class="muted" data-testid="wallet-fills-off">{t('orders.fills.noIndexer')}</p>;
  return (
    <div class="stack-sm" data-testid="wallet-fills" ref={box}>
      {data.loading && !all ? <Loading /> : null}
      <ErrorBanner error={data.error} onRetry={data.reload} />
      {all && phone ? (
        <div class="wf-cards" data-testid="wallet-fills-cards">
          {all.length > 1 ? <SortControl id="wallet-fills" columns={cols} state={props.sort} own={ownCol} onSort={props.onSort} /> : null}
          {list.length === 0 ? <p class="muted center" data-testid="wallet-fills-empty">{t('orders.fills.none')}</p> : null}
          {list.map((item) => {
            const { fill, row } = item;
            const f = factsOf(item);
            return (
              <div key={f.key} class="wf-card" data-testid="wallet-fill-row" data-txid={fill.txid} data-order={row.id}>
                <div class="wf-l1">
                  <span class="bal-asset wf-market">
                    {f.identity}
                    {f.marketHref ? <a href={f.marketHref}>{f.market}</a> : f.market}
                  </span>
                  <span class={`wf-side ${row.side === 'buy' ? 'buy' : 'sell'}`} data-testid="wallet-fill-side">{t(`common.side.${row.side}`)}</span>
                  <span class="wf-price num">{f.price}</span>
                </div>
                <div class="wf-l2">
                  <span class="num">{f.amount} <span class="muted small">{f.ticker}</span></span>
                  <span class="wf-value">
                    <span class="muted small">{t('orders.col.valueKas')}</span> <span class="num">{f.value}</span>
                  </span>
                </div>
                <div class="wf-l3">
                  <span class="wf-time muted small">{f.time}</span>
                  <span class="wf-tx"><CopyText value={fill.txid} head={6} tail={4} href={txUrl(fill.txid)} data-testid="wallet-fill-tx" /></span>
                </div>
              </div>
            );
          })}
        </div>
      ) : null}
      {all && !phone ? (
        <Table dense class="ml-table" data-testid="wallet-fills-table">
          <thead>
            <tr>
              {cols.map((c) => (
                <th key={c.key} class={c.key === 'price' || c.key === 'amount' || c.key === 'value' ? 'right' : undefined} aria-sort={ariaSortOf(props.sort, c.key, ownCol)}>
                  <SortButton id="wallet-fills" col={c} state={props.sort} own={ownCol} onSort={props.onSort} />
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {list.length === 0 ? <TableMessage colSpan={7} data-testid="wallet-fills-empty">{t('orders.fills.none')}</TableMessage> : null}
            {list.map((item) => {
              const { fill, row } = item;
              const f = factsOf(item);
              return (
                <tr key={f.key} data-testid="wallet-fill-row" data-txid={fill.txid} data-order={row.id}>
                  <td class="nowrap">{f.time}</td>
                  <td class="nowrap">
                    <span class="bal-asset">
                      {f.identity}
                      {f.marketHref ? <a href={f.marketHref}>{f.market}</a> : f.market}
                    </span>
                  </td>
                  <td class={row.side === 'buy' ? 'buy' : 'sell'} data-testid="wallet-fill-side">{t(`common.side.${row.side}`)}</td>
                  <td class="right num">{f.price}</td>
                  <td class="right num nowrap">
                    {f.amount} <span class="muted small">{f.ticker}</span>
                  </td>
                  <td class="right num">{f.value}</td>
                  <td>
                    <CopyText value={fill.txid} head={6} tail={4} href={txUrl(fill.txid)} data-testid="wallet-fill-tx" />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
      {pager}
    </div>
  );
}
