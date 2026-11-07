// Who has paid an address, read in the browser from the public Kaspa REST explorer (`EXPLORER_BASE`: api.kaspa.org, api-tn10.kaspa.org; CORS `*`, and the
// paging header below is exposed to the page). The Support page's table (ui/support/SupportPage.tsx) comes from here; the parsing and ranking are pure.
//
// What counts: an accepted transaction with outputs to the address and none of its inputs from it (the address's own spending, change included, is not a
// contribution; a coinbase has no sender). A payment is credited to one sender, the input address that put in the most, as a wallet often spends from several.
// Only addresses are taken from the chain: payloads are not asked for and no free text ever reaches the page.
import { EXPLORER_BASE } from './genesis-check';
import { readCapped, type KascovNetwork } from './kascov';

/** `kaspa:` or `kaspatest:` and a cashaddr body (a pubkey or script address is 61 characters, an ECDSA one 63). Nothing else from a source is shown. */
const ADDRESS = /^kaspa(test)?:[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{61,63}$/;
const HEX64 = /^[0-9a-f]{64}$/;

export const isKaspaAddress = (v: unknown): v is string => typeof v === 'string' && ADDRESS.test(v);

/** The network an address belongs to: its transactions are on that chain, whatever network the app is set to. */
export const networkOfAddress = (address: string): KascovNetwork => (address.startsWith('kaspatest:') ? 'testnet-10' : 'mainnet');

/** Most rows the route gives per request (its own limit). */
export const CONTRIBUTIONS_PAGE = 500;
/** Most requests per read, one after the other: 5,000 transactions. A longer history is cut at its newest ones, said so on the page. */
export const CONTRIBUTIONS_MAX_PAGES = 10;
const PAGE_MAX_BYTES = 8_000_000;
const PAGE_TIMEOUT_MS = 15_000;

export interface Payment {
  txid: string;
  sender: string;
  /** sompi paid to the address by this transaction */
  amount: bigint;
  /** block time, epoch milliseconds */
  time: number;
}

export interface Contributor {
  address: string;
  /** sompi */
  total: bigint;
  /** block time of the first payment, epoch milliseconds */
  first: number;
}

export interface Contributions {
  /** best first (`rankContributors`) */
  contributors: Contributor[];
  /** false: a transaction could not be read, or the history is longer than the requests allowed, so a total may be low */
  complete: boolean;
}

const obj = (v: unknown): Record<string, unknown> | null => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null);

/** An amount in sompi: a JSON number above 2^53 has already lost digits and is not taken. */
function sompi(v: unknown): bigint | null {
  if (typeof v === 'number') return Number.isSafeInteger(v) && v >= 0 ? BigInt(v) : null;
  return typeof v === 'string' && /^\d{1,20}$/.test(v) ? BigInt(v) : null;
}

/** The address that put the most into `inputs` (ties: the lower address), or null when an input is not resolved. */
function senderOf(inputs: readonly unknown[], prefix: string): string | null {
  const put = new Map<string, bigint>();
  for (const raw of inputs) {
    const i = obj(raw);
    const from = i?.previous_outpoint_address;
    const amount = sompi(i?.previous_outpoint_amount);
    if (!isKaspaAddress(from) || !from.startsWith(prefix) || amount === null) return null;
    put.set(from, (put.get(from) ?? 0n) + amount);
  }
  let best: string | null = null;
  for (const [a, v] of put) {
    const b = best === null ? -1n : put.get(best)!;
    if (v > b || (v === b && a < best!)) best = a;
  }
  return best;
}

/** One transaction of the route: its payment, null when it is none, 'unreadable' when it cannot be told. */
export function paymentOf(raw: unknown, address: string): Payment | null | 'unreadable' {
  const tx = obj(raw);
  if (!tx || !HEX64.test(String(tx.transaction_id)) || !Array.isArray(tx.outputs)) return 'unreadable';
  if (tx.is_accepted !== true) return null;
  let amount = 0n;
  for (const raw of tx.outputs) {
    const o = obj(raw);
    if (o?.script_public_key_address !== address) continue;
    const v = sompi(o.amount);
    if (v === null) return 'unreadable';
    amount += v;
  }
  if (amount === 0n) return null;
  const inputs: unknown[] = Array.isArray(tx.inputs) ? tx.inputs : [];
  if (inputs.length === 0 || inputs.some((i) => obj(i)?.previous_outpoint_address === address)) return null;
  const sender = senderOf(inputs, address.slice(0, address.indexOf(':') + 1));
  const time = tx.block_time;
  if (sender === null || typeof time !== 'number' || !Number.isSafeInteger(time) || time <= 0) return 'unreadable';
  return { txid: tx.transaction_id as string, sender, amount, time };
}

/** Totals per sender, best first: by total given, ties by the earlier first contribution (then by address, so the order never depends on the read). */
export function rankContributors(payments: Iterable<Payment>): Contributor[] {
  const by = new Map<string, Contributor>();
  for (const p of payments) {
    const c = by.get(p.sender);
    if (c) {
      c.total += p.amount;
      c.first = Math.min(c.first, p.time);
    } else by.set(p.sender, { address: p.sender, total: p.amount, first: p.time });
  }
  return [...by.values()].sort((a, b) => (a.total === b.total ? a.first - b.first || (a.address < b.address ? -1 : 1) : a.total > b.total ? -1 : 1));
}

/** One page of the route. `resolve_previous_outpoints=light` adds each input's address and amount; `fields` leaves the payload out. */
export function contributionsUrl(address: string, before: string | null): string {
  const q = `limit=${CONTRIBUTIONS_PAGE}&resolve_previous_outpoints=light&acceptance=accepted&fields=transaction_id,block_time,is_accepted,inputs,outputs`;
  return `${EXPLORER_BASE[networkOfAddress(address)]}/addresses/${address}/full-transactions-page?${q}${before === null ? '' : `&before=${before}`}`;
}

async function readPage(url: string, fetchFn: typeof fetch, signal?: AbortSignal): Promise<{ rows: unknown[]; next: string | null }> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), PAGE_TIMEOUT_MS);
  const onAbort = () => ctl.abort();
  signal?.addEventListener('abort', onAbort, { once: true });
  try {
    const res = await fetchFn(url, { signal: ctl.signal, credentials: 'omit', referrerPolicy: 'no-referrer', headers: { accept: 'application/json' } });
    if (!res.ok) throw new Error(`explorer: HTTP ${res.status}`);
    const body = await readCapped(res, PAGE_MAX_BYTES);
    if (!body) throw new Error('explorer: page too large');
    const rows = JSON.parse(new TextDecoder().decode(body)) as unknown;
    if (!Array.isArray(rows)) throw new Error('explorer: not a list');
    const next = res.headers.get('x-next-page-before');
    if (next !== null && !/^\d{1,16}$/.test(next)) throw new Error('explorer: bad cursor');
    return { rows, next };
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', onAbort);
  }
}

/**
 * Walks the address's history, newest first, one request at a time, until the route names no next page or `CONTRIBUTIONS_MAX_PAGES` is reached. The cursor is a
 * block time, so a payment arriving during the read shifts nothing; a transaction seen twice counts once. Rejects when any request fails: a table of half
 * the history would rank people wrongly, so the page shows a line instead.
 */
export async function readContributions(address: string, o: { fetch?: typeof fetch; signal?: AbortSignal } = {}): Promise<Contributions> {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const payments = new Map<string, Payment>();
  let complete = true;
  let before: string | null = null;
  for (let page = 0; ; page++) {
    if (page === CONTRIBUTIONS_MAX_PAGES) {
      complete = false;
      break;
    }
    const { rows, next } = await readPage(contributionsUrl(address, before), fetchFn, o.signal);
    for (const row of rows) {
      const p = paymentOf(row, address);
      if (p === 'unreadable') complete = false;
      else if (p) payments.set(p.txid, p);
    }
    if (next === null) break;
    before = next;
  }
  return { contributors: rankContributors(payments.values()), complete };
}
