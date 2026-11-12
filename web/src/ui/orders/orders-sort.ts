// What the Portfolio's order lists sort by: each column of an order's line, as a value the shared sort (ui/kit/sort.ts) compares.
import { RAW_TOKEN_DECIMALS, t } from '../../i18n';
import { daaToUnix } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { quoteOf } from '../../kob/units';
import type { DtColumn, DtPlain } from '../kit/DataTable';
import { scaledUnits, type SortValue } from '../kit/sort';
import type { OrderRowModel } from './orders-model';

export const ORDER_SORT_KEYS = ['time', 'market', 'type', 'side', 'price', 'avg', 'amount', 'filled', 'total', 'status'] as const;
export type OrderSortKey = (typeof ORDER_SORT_KEYS)[number];

/**
 * The columns of the order tables, in the usual exchange order, the time first. The leading columns are the same in the three Portfolio tables (Trade
 * history has no Type): Time, Market, Type, Side, Price, Amount, Total. Open orders go on with Filled, Status, the buttons; Order history with Avg price, Filled, Status. `time` is the table's own order (newest first).
 */
export function orderColumns(tab: 'open' | 'history', usd = false): (DtColumn<OrderSortKey> | DtPlain)[] {
  const time: DtColumn<OrderSortKey> = { key: 'time', label: t('orders.col.time'), kind: 'time', card: 'badge' };
  const market: DtColumn<OrderSortKey> = { key: 'market', label: t('orders.col.market'), kind: 'text', card: 'title' };
  const type: DtColumn<OrderSortKey> = { key: 'type', label: t('orders.col.type'), kind: 'text', card: 'hidden' };
  const side: DtColumn<OrderSortKey> = { key: 'side', label: t('orders.col.side'), kind: 'text', card: 'hidden' };
  const price: DtColumn<OrderSortKey> = { key: 'price', label: t(usd ? 'orders.col.priceLineUsd' : 'orders.col.priceLine'), kind: 'number', right: true };
  const avg: DtColumn<OrderSortKey> = { key: 'avg', label: t(usd ? 'orders.col.avgPriceUsd' : 'orders.col.avgPrice'), kind: 'number', right: true };
  const amount: DtColumn<OrderSortKey> = { key: 'amount', label: t('orders.col.amount'), kind: 'number', right: true };
  const filled: DtColumn<OrderSortKey> = { key: 'filled', label: t('orders.col.filled'), kind: 'number', right: true };
  const total: DtColumn<OrderSortKey> = { key: 'total', label: t(tab === 'history' ? (usd ? 'orders.col.tradedUsd' : 'orders.col.traded') : usd ? 'orders.col.totalUsd' : 'orders.col.total'), kind: 'number', right: true };
  const status: DtColumn<OrderSortKey> = { key: 'status', label: t('orders.col.status'), kind: 'text' };
  return tab === 'history'
    ? [time, market, type, side, price, amount, total, avg, filled, status, { key: 'toggle', plain: true }]
    : [time, market, type, side, price, amount, total, filled, status, { key: 'act', plain: true }, { key: 'toggle', plain: true }];
}

/** When the order was placed, unix seconds: the wallet's record of it, else the time of its genesis block (null: unknown). */
export function placedUnix(row: OrderRowModel, clock: Clock | null | undefined): bigint | null {
  const genesis = row.entry.view ? BigInt(row.entry.view.genesis.daa) : null;
  const placedRecord = row.entry.record?.placedAtUnix ? BigInt(row.entry.record.placedAtUnix) : 0n;
  return placedRecord > 0n ? placedRecord : genesis !== null && clock ? daaToUnix(clock, genesis) : null;
}

/**
 * The filled amount of an order, base units (null: unknown). The indexer's `filled_amount` is what the fills took; `amount_left` is 0 for every ended order but a
 * killed one, so `total - left` reads an order cancelled after a partial fill as 100 % filled. Only a row without the field (a node-read or recorded order) falls back to it.
 */
export function filledAmount(row: OrderRowModel): bigint | null {
  const f = row.entry.view?.filled_amount;
  if (typeof f === 'string' && /^\d+$/.test(f)) return BigInt(f);
  if (row.amountTotal === null || row.amountLeft === null) return null;
  return row.amountTotal > row.amountLeft ? row.amountTotal - row.amountLeft : 0n;
}

/** The filled part of an order in hundredths of a percent (null: a KAS budget, which has no amount to compare). */
export function filledPermyriad(row: OrderRowModel): number | null {
  const total = row.amountTotal;
  const filled = filledAmount(row);
  if (total === null || filled === null || total <= 0n) return null;
  return Number(((filled > total ? total : filled) * 10_000n) / total);
}

/**
 * The KAS a plain bid has spent on its fills when its placement is not in this browser (no budget record): the filled tokens at the order's average fill price (null: not a bid
 * with a budget, nothing filled, or no average price yet). `filled_amount` is a decimal string.
 */
export function bidSpentByFills(row: OrderRowModel, avg: bigint | null | undefined): bigint | null {
  const filled = filledAmount(row) ?? 0n;
  return row.budget && !row.pair && avg !== null && avg !== undefined && filled > 0n ? quoteOf(filled, avg, row.scale, 'down') : null;
}

/** An order that is over (filled, cancelled, refunded, killed or closed): its fills are all it ever did. */
export const orderEnded = (row: OrderRowModel): boolean => !row.live && row.status !== 'unknown';

/**
 * The Total column, KAS (null: none). An open order: its initial amount at its limit price (what it offers). An ended order: what actually traded, the filled amount at the
 * average fill price `avg` (the order's own fills, read from its events; null / undefined until they are: no figure yet), 0 when nothing filled. A pair order has none.
 */
export function orderTotalKas(row: OrderRowModel, avg: bigint | null | undefined): bigint | null {
  if (row.pair) return null;
  if (orderEnded(row)) {
    const filled = filledAmount(row);
    if (filled === null) return null;
    if (filled === 0n) return 0n;
    return avg !== null && avg !== undefined ? quoteOf(filled, avg, row.scale, 'down') : null;
  }
  return row.amountTotal !== null && row.price !== null ? quoteOf(row.amountTotal, row.price, row.scale, 'down') : null;
}

/** The value of one column for an order. `marketText`: the market as the line names it; `decimalsOf`: the decimals of the order's token; `avgOf`: its average fill price (null: not known). */
export function orderSortValue(
  row: OrderRowModel,
  key: OrderSortKey,
  ctx: { clock: Clock | null | undefined; marketText: (row: OrderRowModel) => string; decimalsOf: (row: OrderRowModel) => number | undefined; avgOf: (row: OrderRowModel) => bigint | null },
): SortValue {
  const decimals = ctx.decimalsOf(row) ?? RAW_TOKEN_DECIMALS;
  switch (key) {
    case 'time':
      return placedUnix(row, ctx.clock);
    case 'market':
      return ctx.marketText(row);
    case 'type':
      return t(`orders.typeShort.${row.typeKey}`);
    case 'side':
      return t(`common.side.${row.side}`);
    case 'price':
      return row.price;
    case 'avg':
      return ctx.avgOf(row);
    case 'amount':
      return row.amountTotal !== null ? scaledUnits(row.amountTotal, decimals) : null;
    case 'filled': {
      const v = filledAmount(row);
      return v !== null ? scaledUnits(v, decimals) : null;
    }
    case 'total':
      return orderTotalKas(row, ctx.avgOf(row));
    case 'status':
      return t(`orders.status.${row.status}`);
  }
}
