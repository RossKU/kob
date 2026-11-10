// What the Portfolio's order lists sort by: each column of an order's line, as a value the shared sort (ui/kit/sort.ts) compares.
import { t } from '../../i18n';
import { daaToUnix } from '../../kob/daa';
import type { Clock } from '../../kob/plan-types';
import { quoteOf } from '../../kob/units';
import type { SortColumn, SortValue } from '../kit/sort';
import type { OrderRowModel } from './orders-model';

export const ORDER_SORT_KEYS = ['time', 'market', 'type', 'side', 'price', 'trigger', 'filled', 'value', 'status'] as const;
export type OrderSortKey = (typeof ORDER_SORT_KEYS)[number];

/** The columns of the order lists, in the order of the line; `time` is what the list's own order (newest first) stands for. */
export function orderColumns(): SortColumn<OrderSortKey>[] {
  return [
    { key: 'time', label: t('orders.col.time'), kind: 'time' },
    { key: 'market', label: t('orders.col.market'), kind: 'text' },
    { key: 'type', label: t('orders.col.type'), kind: 'text' },
    { key: 'side', label: t('orders.col.side'), kind: 'text' },
    { key: 'price', label: t('orders.col.priceLine'), kind: 'number' },
    { key: 'trigger', label: t('orders.col.trigger'), kind: 'number' },
    { key: 'filled', label: t('orders.col.filled'), kind: 'number' },
    { key: 'value', label: t('orders.col.valueKas'), kind: 'number' },
    { key: 'status', label: t('orders.col.status'), kind: 'text' },
  ];
}

/** When the order was placed, unix seconds: the wallet's record of it, else the time of its genesis block (null: unknown). */
export function placedUnix(row: OrderRowModel, clock: Clock | null | undefined): bigint | null {
  const genesis = row.entry.view ? BigInt(row.entry.view.genesis.daa) : null;
  const placedRecord = row.entry.record?.placedAtUnix ? BigInt(row.entry.record.placedAtUnix) : 0n;
  return placedRecord > 0n ? placedRecord : genesis !== null && clock ? daaToUnix(clock, genesis) : null;
}

/** The filled part of an order in hundredths of a percent (null: a KAS budget, which has no amount to compare). */
export function filledPermyriad(row: OrderRowModel): number | null {
  const total = row.amountTotal;
  const left = row.amountLeft;
  if (total === null || left === null || total <= 0n) return null;
  const filled = total > left ? total - left : 0n;
  return Number((filled * 10_000n) / total);
}

/** The value of one column for an order (the text of a column is the text the line shows). `marketText`: the market as the line names it. */
export function orderSortValue(row: OrderRowModel, key: OrderSortKey, ctx: { clock: Clock | null | undefined; marketText: (row: OrderRowModel) => string }): SortValue {
  switch (key) {
    case 'time':
      return placedUnix(row, ctx.clock);
    case 'market':
      return ctx.marketText(row);
    case 'type':
      return t(`orders.type.${row.typeKey}`);
    case 'side':
      return t(`common.side.${row.side}`);
    case 'price':
      return row.price;
    case 'trigger':
      return row.stopPrice;
    case 'filled':
      return filledPermyriad(row);
    case 'value':
      return !row.pair && row.amountTotal !== null && row.price !== null ? quoteOf(row.amountTotal, row.price, row.scale, 'down') : null;
    case 'status':
      return t(`orders.status.${row.status}`);
  }
}
