// Fill history of one order on demand (R-8): every fill with its time, amount, price and payout, the average price, and a CSV download.
import { kasValueText } from '../../app/fiat';
import { useFiat } from '../../app/use-fiat';
import { priceUsdText } from './fiat-price';
import { useState } from 'preact/hooks';
import type { EventView } from '../../data/indexer-types';
import { useTxUrl } from '../../app/explorer';
import { has, t, RAW_TOKEN_DECIMALS } from '../../i18n';
import type { TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { formatKas } from '../../kob/units';
import { Button, CopyText, ErrorBanner, displayPriceText, exactPriceText, formatRowTimeUnix, Loading, rowTimeTitleUnix, Table, useAsync } from '../kit';
import { downloadText } from '../kit/download';
import { fillHistory, fillsCsv, fillsFileName } from './fills-model';
import type { OrderRowModel } from './orders-model';
import { amountCell, priceDpOf, tokenUnit } from './cells';
import { countText } from './plural';

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
  // the USD display: a price and the payout in dollars
  const fiat = useFiat();
  const data = useAsync(async (signal) => (open ? fillHistory(await props.loadEvents(row.id, signal), row.scale) : null), [open, row.id]);
  const decimals = token?.decimals ?? RAW_TOKEN_DECIMALS;
  // prices are the order's state prices (sompi per whole token of `row.scale` base units). One number of decimals for the whole list and the average (the finest any of them needs,
  // 5 significant digits, at most 6), the exact price in the title: the decimal points line up and no price is cut without saying so
  const priceDp = priceDpOf([...(data.data?.rows ?? []).map((r) => r.price), data.data?.avgPrice].map((price) => ({ price, decimals, scale: row.scale })));
  const unit = tokenUnit(token?.ticker, row.token);
  const kasPer = t('orders.unit.kasPer', { ticker: unit.text || '?' });
  const priceCell = (p: bigint) => (fiat && !row.pair ? priceUsdText(p, decimals, row.scale, fiat) : displayPriceText(p, decimals, row.scale, priceDp));
  const priceTitle = (p: bigint) => (fiat && !row.pair ? undefined : `${exactPriceText(p, decimals, row.scale)} ${kasPer}`);
  const price = (p: bigint) => (fiat && !row.pair ? priceUsdText(p, decimals, row.scale, fiat) : `${priceCell(p)} ${kasPer}`);
  const amountText = (a: bigint) => amountCell(a, decimals).text;
  // a pair order: its fills are volume only (amounts of A and B, how it was filled), never a price
  const pairOrder = !!row.pair;
  const quote = props.quote;
  const bText = (b: bigint | null | undefined) => (b === null || b === undefined ? '—' : quote ? amountCell(b, quote.decimals).text : b.toString());
  const h = data.data;
  const download = () => {
    if (!h) return;
    const csv = fillsCsv([{ info: { ticker: token?.ticker ?? row.token ?? '', side: row.side, type: row.typeKey, ...(token ? { decimals: token.decimals } : {}) }, history: h }]);
    downloadText(fillsFileName(props.network), csv, 'text/csv');
  };
  return (
    <div class="order-fills small" data-testid={`order-fills-${row.id}`}>
      <Button small variant="ghost" onClick={() => setOpen((o) => !o)} aria-expanded={open} data-testid={`order-fills-toggle-${row.id}`}>
        {t(open ? 'orders.fills.hide' : 'orders.fills.show', { amount: `${amountText(filledAmountOf(row))} ${unit.text}`.trim() })}
      </Button>
      {open ? (
        <div class="stack-sm" style="margin-top:6px">
          {data.loading && !h ? <Loading /> : null}
          <ErrorBanner error={data.error} onRetry={data.reload} />
          {h ? (
            <>
              <p data-testid="order-fills-summary">
                {t('orders.fills.summary', { fills: countText('fill', h.rows.length), amount: `${amountText(h.amount)} ${unit.text}`.trim() })}
                {h.avgPrice !== null ? <span title={priceTitle(h.avgPrice)}>{` · ${t('orders.fills.avg', { price: price(h.avgPrice) })}`}</span> : ''}
                {h.payout > 0n ? ` · ${fiat ? t('orders.fills.payoutUsd', { usd: kasValueText(h.payout, fiat) }) : t('orders.fills.payout', { kas: formatKas(h.payout) })}` : ''}
              </p>
              {h.rows.length ? (
                <Table dense data-testid="order-fills-table">
                  <thead>
                    <tr>
                      <th>{t('orders.fills.time')}</th>
                      <th class="right">{t('orders.fills.amountIn', { unit: unit.text || '?' })}</th>
                      {pairOrder ? (
                        <>
                          <th class="right">{t('orders.fills.amountIn', { unit: quote?.ticker ?? '?' })}</th>
                          <th>{t('orders.fills.via')}</th>
                        </>
                      ) : (
                        <th class="right">{t('orders.fills.priceIn', { unit: t(fiat ? 'orders.unit.usdPer' : 'orders.unit.kasPer', { ticker: unit.text || '?' }) })}</th>
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
                        <td title={r.atMs > 0 ? rowTimeTitleUnix(Math.floor(r.atMs / 1000)) : undefined}>{r.atMs > 0 ? formatRowTimeUnix(Math.floor(r.atMs / 1000)) : '—'}</td>
                        <td class="right num" title={amountCell(r.amount, decimals).title}>{amountText(r.amount)}</td>
                        {pairOrder ? (
                          <>
                            <td class="right num" data-testid="order-fill-b">{bText(r.pair?.amountB)}</td>
                            <td data-testid="order-fill-via">{r.pair ? (has(`pair.trades.cp.${r.pair.counterparty}`) ? t(`pair.trades.cp.${r.pair.counterparty}`) : r.pair.counterparty) : '—'}</td>
                          </>
                        ) : (
                          <td class="right num" title={r.price !== null ? priceTitle(r.price) : undefined}>{r.price !== null ? priceCell(r.price) : '—'}</td>
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
