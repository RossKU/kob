// Fill history of one order (R-8): per-fill rows from the indexer's order events (`/v1/orders/{id}/events`), the amount-weighted average price, and a
// CSV export of the wallet's fills. Pure: no network, no DOM (the caller loads the events and triggers the download).
//
// Amounts are token base units (event `amount`), prices state prices (sompi per whole token of the order's `scale` base units, event `price`).
import type { EventView } from '../../data/indexer-types';
import { pairDetailOf } from '../../data/pair-api';
import type { Hex } from '../../kob/types';
import { formatPricePerToken, formatUnits, quoteOf } from '../../kob/units';

export interface FillRow {
  orderId: Hex;
  /** block time, unix milliseconds (0 when unknown) */
  atMs: number;
  txid: Hex;
  /** base units filled */
  amount: bigint;
  /** the fill's price, sompi per whole token of the order's scale; null when the event carries none */
  price: bigint | null;
  /** KAS paid out to the maker by this fill (sell side), sompi */
  payout: bigint;
  settled: boolean;
  /**
   * a pair order's fill (event `detail.pair`): the quote token B the maker received (an ask) or paid (a bid), base units, and how it was filled
   * (`route`, `netting`, `inventory`). A pair fill carries no KAS price (`price` null): it is volume only.
   */
  pair?: { amountB: bigint | null; counterparty: string };
}

export interface FillHistory {
  rows: FillRow[];
  /** base units filled over all rows */
  amount: bigint;
  /** amount-weighted average price (sompi per whole token, rounded down); null without a priced fill */
  avgPrice: bigint | null;
  payout: bigint;
  /** the order's scale the prices are quoted per */
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

/** The fills of one order's events, oldest first; duplicate events (same id) count once. `scale` is the order's (its prices' denominator). */
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
  /** the token's decimals (amounts in token units; base units when unknown) */
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
 * CSV of fills (RFC 4180, UTC ISO times, KAS with 8 decimals, exact): one line per fill of every order given. `amount` is in token units when the
 * decimals are known (else base units), `price_kas_per_token` KAS per whole token, `value_kas` the fill's value at its price (rounded down). Text
 * cells that a spreadsheet would read as a formula are quoted (no CSV injection through a token ticker).
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

/** `kob-fills-<network>-<yyyymmdd>.csv` (UTC date). */
export function fillsFileName(network: string, now: Date | number = new Date()): string {
  const d = new Date(now);
  const ymd = `${d.getUTCFullYear()}${String(d.getUTCMonth() + 1).padStart(2, '0')}${String(d.getUTCDate()).padStart(2, '0')}`;
  return `kob-fills-${network.replace(/[^a-z0-9-]+/gi, '-').toLowerCase()}-${ymd}.csv`;
}
