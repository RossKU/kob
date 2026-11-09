import { useMemo } from 'preact/hooks';
import type { EventView } from '../../data/indexer-types';
import { useTxUrl } from '../../app/explorer';
import { pairRoute, routeToHash } from '../../app/router';
import { formatDateTime, t, RAW_TOKEN_DECIMALS } from '../../i18n';
import { longId, type TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { formatKas, formatUnits, quoteOf } from '../../kob/units';
import { CopyText, ErrorBanner, fixedPricePerToken, Loading, Table, TableMessage, tickPriceFraction, useAsync } from '../kit';
import { IdentityBlock } from '../market/Fingerprint';
import { fillHistory, type FillRow } from './fills-model';
import type { OrderRowModel } from './orders-model';

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
  const list = data.data;
  const quoteOfPair = useMemo(() => new Map<string, TokenInfo | undefined>(), []);

  if (!props.loadEvents) return <p class="muted" data-testid="wallet-fills-off">{t('orders.fills.noIndexer')}</p>;
  return (
    <div class="stack-sm" data-testid="wallet-fills">
      {data.loading && !list ? <Loading /> : null}
      <ErrorBanner error={data.error} onRetry={data.reload} />
      {list ? (
        <Table dense class="ml-table" data-testid="wallet-fills-table">
          <thead>
            <tr>
              <th>{t('orders.fills.time')}</th>
              <th>{t('orders.col.market')}</th>
              <th>{t('orders.col.side')}</th>
              <th class="right">{t('orders.col.price')}</th>
              <th class="right">{t('orders.col.amount')}</th>
              <th class="right">{t('orders.col.valueKas')}</th>
              <th>{t('orders.fills.tx')}</th>
            </tr>
          </thead>
          <tbody>
            {list.length === 0 ? <TableMessage colSpan={7} data-testid="wallet-fills-empty">{t('orders.fills.none')}</TableMessage> : null}
            {list.map(({ fill, row, token }) => {
              const href = txUrl(fill.txid);
              const decimals = token?.decimals ?? RAW_TOKEN_DECIMALS;
              const ticker = token?.ticker ?? (row.token ? longId(row.token) : '?');
              let quote: TokenInfo | undefined;
              if (row.pair) {
                if (!quoteOfPair.has(row.pair.quote)) quoteOfPair.set(row.pair.quote, props.tokenOf(row.pair.quote, row.pair.quoteScale));
                quote = quoteOfPair.get(row.pair.quote);
              }
              const market = row.pair ? `${ticker}/${quote?.ticker ?? longId(row.pair.quote)}` : `${ticker}/KAS`;
              const marketHref = row.pair ? routeToHash(pairRoute(row.pair.base, row.pair.quote)) : row.token ? `#/market/${row.token}` : undefined;
              // a KAS market's fill has a price and a KAS value; a pair fill is volume only (the B amount stands for its value)
              const price = !row.pair && fill.price !== null ? fixedPricePerToken(fill.price, decimals, row.scale, (token ? tickPriceFraction(token.tick, decimals, row.scale) : null) ?? 8) : '—';
              const value = !row.pair && fill.price !== null ? formatKas(quoteOf(fill.amount, fill.price, row.scale, 'down'), { group: ',' }) : '—';
              return (
                <tr key={`${fill.txid}-${row.id}-${fill.atMs}`} data-testid="wallet-fill-row" data-txid={fill.txid} data-order={row.id}>
                  <td class="nowrap">{fill.atMs > 0 ? formatDateTime(Math.floor(fill.atMs / 1000), { dateStyle: 'short', timeStyle: 'medium' }) : '—'}</td>
                  <td class="nowrap">
                    <span class="bal-asset">
                      {row.token ? <IdentityBlock row={{ covenantId: row.token, ticker: token?.ticker ?? '', kascov: null }} size="sm" /> : null}
                      {marketHref ? <a href={marketHref}>{market}</a> : market}
                    </span>
                  </td>
                  <td class={row.side === 'buy' ? 'buy' : 'sell'} data-testid="wallet-fill-side">{t(`common.side.${row.side}`)}</td>
                  <td class="right num">{price}</td>
                  <td class="right num nowrap">
                    {formatUnits(fill.amount, decimals, { group: ',' })} <span class="muted small">{ticker}</span>
                  </td>
                  <td class="right num">{value}</td>
                  <td>
                    <CopyText value={fill.txid} head={6} tail={4} href={href} />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
    </div>
  );
}
