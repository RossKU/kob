// Fill history of one order on demand (R-8): every fill with its time, amount, price and payout, the average price, and a CSV download.
import { useState } from 'preact/hooks';
import type { EventView } from '../../data/indexer-types';
import { useTxUrl } from '../../app/explorer';
import { formatDateTime, has, t } from '../../i18n';
import type { TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { formatKas, formatPricePerToken, formatUnits } from '../../kob/units';
import { Button, CopyText, ErrorBanner, fixedPricePerToken, fixedUnits, Loading, Table, tickPriceFraction, useAsync } from '../kit';
import { downloadText } from '../kit/download';
import { fillHistory, fillsCsv, fillsFileName } from './fills-model';
import type { OrderRowModel } from './orders-model';

export interface OrderFillsProps {
  row: OrderRowModel;
  token: TokenInfo | undefined;
  /** a pair order's quote token B (its fills show the B amount and the counterparty, no price) */
  quote?: TokenInfo | undefined;
  network: string;
  loadEvents(id: Hex, signal: AbortSignal): Promise<EventView[]>;
}

/** Base units this order has filled so far (indexer view), 0 when unknown. */
export const filledAmountOf = (row: OrderRowModel): bigint => {
  const v = row.entry.view?.filled_amount;
  return typeof v === 'string' && /^\d+$/.test(v) ? BigInt(v) : 0n;
};

export function OrderFills(props: OrderFillsProps) {
  const { row, token } = props;
  const [open, setOpen] = useState(false);
  const txUrl = useTxUrl();
  const data = useAsync(async (signal) => (open ? fillHistory(await props.loadEvents(row.id, signal), row.scale) : null), [open, row.id]);
  // prices are the order's state prices (sompi per whole token of `row.scale` base units)
  const price = (p: bigint) =>
    token ? `${formatPricePerToken(p, token.decimals, row.scale)} KAS/${token.ticker}` : `${formatKas(p)} KAS ${t('orders.perScale', { scale: row.scale.toString() })}`;
  // the price column: fixed decimals for every row (from the token's tick, else 8), the unit in the header (decimal points line up)
  const priceFraction = token ? (tickPriceFraction(token.tick, token.decimals, row.scale) ?? 8) : 8;
  const priceCell = (p: bigint) => (token ? fixedPricePerToken(p, token.decimals, row.scale, priceFraction) : fixedUnits(p, 8, priceFraction));
  const amountText = (a: bigint) => (token ? formatUnits(a, token.decimals, { group: ',' }) : a.toString());
  // a pair order: its fills are volume only (amounts of A and B, how it was filled), never a price
  const pairOrder = !!row.pair;
  const quote = props.quote;
  const bText = (b: bigint | null | undefined) => (b === null || b === undefined ? '—' : quote ? formatUnits(b, quote.decimals, { group: ',' }) : b.toString());
  const h = data.data;
  const download = () => {
    if (!h) return;
    const csv = fillsCsv([{ info: { ticker: token?.ticker ?? row.token ?? '', side: row.side, type: row.typeKey, ...(token ? { decimals: token.decimals } : {}) }, history: h }]);
    downloadText(fillsFileName(props.network), csv, 'text/csv');
  };
  return (
    <div class="order-fills small" data-testid={`order-fills-${row.id}`}>
      <Button small variant="ghost" onClick={() => setOpen((o) => !o)} aria-expanded={open} data-testid={`order-fills-toggle-${row.id}`}>
        {t(open ? 'orders.fills.hide' : 'orders.fills.show', { amount: `${amountText(filledAmountOf(row))} ${token?.ticker ?? ''}`.trim() })}
      </Button>
      {open ? (
        <div class="stack-sm" style="margin-top:6px">
          {data.loading && !h ? <Loading /> : null}
          <ErrorBanner error={data.error} onRetry={data.reload} />
          {h ? (
            <>
              <p data-testid="order-fills-summary">
                {t('orders.fills.summary', { fills: h.rows.length, amount: `${amountText(h.amount)} ${token?.ticker ?? ''}`.trim() })}
                {h.avgPrice !== null ? ` · ${t('orders.fills.avg', { price: price(h.avgPrice) })}` : ''}
                {h.payout > 0n ? ` · ${t('orders.fills.payout', { kas: formatKas(h.payout) })}` : ''}
              </p>
              {h.rows.length ? (
                <Table dense data-testid="order-fills-table">
                  <thead>
                    <tr>
                      <th>{t('orders.fills.time')}</th>
                      <th class="right">{t('orders.fills.amountIn', { unit: token?.ticker ?? t('common.baseUnits') })}</th>
                      {pairOrder ? (
                        <>
                          <th class="right">{t('orders.fills.amountIn', { unit: quote?.ticker ?? t('common.baseUnits') })}</th>
                          <th>{t('orders.fills.via')}</th>
                        </>
                      ) : (
                        <th class="right">{t('orders.fills.priceIn', { unit: token ? `KAS/${token.ticker}` : `KAS ${t('orders.perScale', { scale: row.scale.toString() })}` })}</th>
                      )}
                      <th>{t('orders.fills.tx')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {h.rows.map((r) => {
                      const href = txUrl(r.txid);
                      // a click anywhere on the row opens the fill's transaction on the block explorer, in a new tab (the id itself is a link too)
                      const open = (e: MouseEvent) => {
                        if (href && !(e.target as HTMLElement).closest('a, button')) window.open(href, '_blank', 'noopener,noreferrer');
                      };
                      return (
                      <tr key={`${r.txid}-${r.atMs}`} data-testid="order-fill-row" data-txid={r.txid} class={href ? 'row-link' : undefined} onClick={open} title={href ? t('orders.explorerHint') : undefined}>
                        <td>{r.atMs > 0 ? formatDateTime(Math.floor(r.atMs / 1000), { dateStyle: 'short', timeStyle: 'medium' }) : '—'}</td>
                        <td class="right num">{amountText(r.amount)}</td>
                        {pairOrder ? (
                          <>
                            <td class="right num" data-testid="order-fill-b">{bText(r.pair?.amountB)}</td>
                            <td data-testid="order-fill-via">{r.pair ? (has(`pair.trades.cp.${r.pair.counterparty}`) ? t(`pair.trades.cp.${r.pair.counterparty}`) : r.pair.counterparty) : '—'}</td>
                          </>
                        ) : (
                          <td class="right num">{r.price !== null ? priceCell(r.price) : '—'}</td>
                        )}
                        <td>
                          <CopyText value={r.txid} head={6} tail={4} href={href} />
                        </td>
                      </tr>
                      );
                    })}
                  </tbody>
                </Table>
              ) : null}
              {h.rows.length ? (
                <Button small onClick={download} data-testid={`order-fills-csv-${row.id}`}>
                  {t('orders.fills.csv')}
                </Button>
              ) : null}
            </>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
