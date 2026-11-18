// Fill history of one order. It has per-fill rows from the indexer's order events
// (`/v1/orders/{id}/events`), the amount-weighted average price and a CSV export of the wallet's
// fills. It is pure, with no network and no DOM. The caller loads the events and triggers the
// download.
//
// Amounts are token base units (event `amount`). Prices are state prices, in sompi per whole token
// of the order's `scale` base units (event `price`).
import type { EventView } from '../../data/indexer-types';
import { pairDetailOf } from '../../data/pair-api';
import type { Hex } from '../../kob/types';
import { formatPricePerToken, formatUnits, quoteOf } from '../../kob/units';

export interface FillRow {
  orderId: Hex;
  /** Block time in unix milliseconds, 0 when unknown. */
  atMs: number;
  txid: Hex;
  /** Base units filled. */
  amount: bigint;
  /**
   * The fill's price in sompi per whole token of the order's scale. Null when the event carries
   * none.
   */
  price: bigint | null;
  /** KAS paid out to the maker by this fill, on the sell side, in sompi. */
  payout: bigint;
  settled: boolean;
  /**
   * A pair order's fill (event `detail.pair`). It has the quote token B the maker received (an ask)
   * or paid (a bid), in base units, and how it was filled (`route`, `netting`, `inventory`). A pair
   * fill carries no KAS price (`price` is null). It is volume only.
   */
  pair?: { amountB: bigint | null; counterparty: string };
}

export interface FillHistory {
  rows: FillRow[];
  /** Base units filled over all rows. */
  amount: bigint;
  /**
   * Amount-weighted average price in sompi per whole token, rounded down. Null without a priced
   * fill.
   */
  avgPrice: bigint | null;
  payout: bigint;
  /** The order's scale the prices are quoted per. */
  scale: bigint;
}

const big = (v: string | number | null | undefined): bigint | null => {
  if (v === null || v === undefined || v === '') return null;
  try {
    return BigInt(v);
  } catch {
    return null;
  }
};

/**
 * The fills of one order's events, oldest first. Duplicate events with the same id count once.
 * `scale` is the order's, the denominator of its prices.
 */
export function fillHistory(events: readonly EventView[], scale: bigint | number = 1): FillHistory {
  const sc = BigInt(scale) > 0n ? BigInt(scale) : 1n;
  const seen = new Set<number>();
  const rows: FillRow[] = [];
  for (const e of events) {
    const amount = big(e.amount);
    if (e.kind !== 'fill' || amount === null || amount <= 0n || seen.has(e.id)) continue;
    seen.add(e.id);
    const pd = pairDetailOf(e);
    rows.push({
      orderId: e.covenant_id, atMs: e.ts > 0 ? e.ts : 0, txid: e.txid, amount, price: pd ? null : big(e.price), payout: big(e.payout) ?? 0n, settled: e.settled,
      ...(pd ? { pair: { amountB: big(pd.amount_b), counterparty: pd.counterparty } } : {}),
    });
  }
  rows.sort((a, b) => a.atMs - b.atMs || (a.txid < b.txid ? -1 : a.txid > b.txid ? 1 : 0));
  let amount = 0n;
  let priced = 0n;
  let value = 0n;
  let payout = 0n;
  for (const r of rows) {
    amount += r.amount;
    payout += r.payout;
    if (r.price !== null) {
      value += r.price * r.amount;
      priced += r.amount;
    }
  }
  return { rows, amount, avgPrice: priced > 0n ? value / priced : null, payout, scale: sc };
}

export interface CsvOrderInfo {
  ticker: string;
  side: 'sell' | 'buy';
  type: string;
  /** The token's decimals. Amounts are in token units, or in base units when unknown. */
  decimals?: number;
}

const SOMPI = 100_000_000n;
const kas = (v: bigint): string => {
  const neg = v < 0n;
  const a = neg ? -v : v;
  const frac = (a % SOMPI).toString().padStart(8, '0');
  return `${neg ? '-' : ''}${a / SOMPI}.${frac}`;
};
const cell = (s: string): string => (/[",\n\r]/.test(s) || /^[=+\-@]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);

/**
 * CSV of fills, with one line per fill of every order given. It follows RFC 4180, uses UTC ISO
 * times and writes KAS exactly with 8 decimals. `amount` is in token units when the decimals are
 * known, else in base units. `price_kas_per_token` is KAS per whole token, and `value_kas` is the
 * fill's value at its price, rounded down. Text cells that a spreadsheet would read as a formula
 * are quoted, so a token ticker cannot inject a formula.
 */
export function fillsCsv(orders: readonly { info: CsvOrderInfo; history: FillHistory }[]): string {
  const head = ['time_utc', 'order_id', 'token', 'side', 'type', 'amount', 'price_kas_per_token', 'value_kas', 'payout_kas', 'txid', 'settled'];
  const lines = [head.join(',')];
  const all = orders.flatMap((o) => o.history.rows.map((r) => ({ r, info: o.info, scale: o.history.scale })));
  all.sort((a, b) => a.r.atMs - b.r.atMs || (a.r.txid < b.r.txid ? -1 : a.r.txid > b.r.txid ? 1 : 0));
  for (const { r, info, scale } of all) {
    const dec = info.decimals;
    lines.push([
      r.atMs > 0 ? new Date(r.atMs).toISOString() : '',
      r.orderId,
      cell(info.ticker),
      info.side,
      cell(info.type),
      dec !== undefined ? formatUnits(r.amount, dec) : r.amount.toString(),
      r.price !== null ? (dec !== undefined ? formatPricePerToken(r.price, dec, scale, { trim: false }) : kas(r.price)) : '',
      r.price !== null ? kas(quoteOf(r.amount, r.price, scale, 'down')) : '',
      kas(r.payout),
      r.txid,
      r.settled ? 'yes' : 'no',
    ].join(','));
  }
  return lines.join('\r\n') + '\r\n';
}

/** The file name `kob-fills-<network>-<yyyymmdd>.csv`, with a UTC date. */
export function fillsFileName(network: string, now: Date | number = new Date()): string {
  const d = new Date(now);
  const ymd = `${d.getUTCFullYear()}${String(d.getUTCMonth() + 1).padStart(2, '0')}${String(d.getUTCDate()).padStart(2, '0')}`;
  return `kob-fills-${network.replace(/[^a-z0-9-]+/gi, '-').toLowerCase()}-${ymd}.csv`;
}
