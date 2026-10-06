// Notification model of the web wallet (pure: no DOM, no network, no timers). Everything is derived on the client from what the indexer already tells the
// wallet about its own orders (`OrderView`), from the wallet's incoming UTXOs and from the status of x402 invoices the user chose to watch.
//
//   snapshotOf(view, clock)         one order -> `OrderSnap` (status, filled amount, armed, auction, expiry, refundable)
//   diffOrders(prev, next, nowMs)   two snapshot maps -> events (fill, partial, armed, triggered, expirySoon, expired, refunded, killed, cancelled)
//   diffVanished(prev, id, nowMs)   a live order the indexer no longer knows -> `reorg` event (its placement was reverted)
//   diffPayments(prev, next, ...)   incoming P2PK KAS / token outputs -> `payment` events (once per outpoint)
//   diffInvoice(prev, status, ...)  a watched invoice's status change -> `invoice` event
//   NotificationStore               per network + wallet persistence (localStorage, in memory when it throws): snapshots, history (200), dedup, invoices
//   NotifySettings                  opt-in switches (`kob.notify.settings`): in-app OFF by default, browser OFF by default, every kind on except `cancelled`
//
// The FIRST poll of a wallet (no stored snapshots) only records the baseline: history never turns into a flood of "filled" notices. Snapshots persist, so
// a fill that happened while the tab was closed is reported on the next load. Event ids are stable (`kind:order:value`), so a repeat is dropped by the store.
// A pair order's fills are VOLUME only (base units of A; `quote` names B): never a price (prices come only from KAS-book fills).
import type { OrderView } from '../data/indexer-types';
import { daaToUnix } from './daa';
import { pairOfView } from './pair-view';
import type { Clock } from './plan-types';
import type { Hex } from './types';

// ------------------------------------------------------------------------------------------------ kinds and events

export const NOTIFY_KINDS = ['fill', 'partial', 'armed', 'triggered', 'expirySoon', 'expired', 'refunded', 'killed', 'cancelled', 'reorg', 'payment', 'invoice'] as const;
export type NotifyKind = (typeof NOTIFY_KINDS)[number];

export type NotifyParams = Record<string, string | number>;

export interface NotificationEvent {
  /** stable: the same fact always yields the same id, so duplicates across polls and reloads are dropped */
  id: string;
  kind: NotifyKind;
  orderId?: Hex;
  /** token covenant id (orders, token payments) */
  token?: Hex;
  side?: 'sell' | 'buy';
  /** plain strings / numbers (amounts are base-unit decimal strings): rendered with i18n at display time */
  params: NotifyParams;
  /** unix ms */
  at: number;
  read: boolean;
}

/** An order within this long of its expiry gets one `expirySoon` (per expiry value): 24 h for GTC / GTD, 1 h for a day order (seconds). */
export const EXPIRY_SOON_SECONDS = 24 * 3600;
export const EXPIRY_SOON_DAY_SECONDS = 3600;
/** Above this an expiry DAA is the "no date" sentinel (2^62) of an exit, not a date (same bound as orders-model). */
const NO_DATE_DAA = 1n << 60n;

// ------------------------------------------------------------------------------------------------ order snapshots

export interface OrderSnap {
  status: string;
  /** base units filled (a decimal string; a snapshot stored by an older build may hold a number) */
  filled: string;
  /** base units left, null when unknown */
  remaining: string | null;
  /** base units the order started with (or filled + remaining), null when unknown */
  total: string | null;
  /** `armed` of the proven state of a conditional kind / stop entry (null: the kind has none or the state is not proven) */
  armed: bigint | null;
  auctionActive: boolean;
  /** unix seconds of the on-chain expiry / the day-order deadline (null: none, a no-date exit, or no clock) */
  expiryAt: number | null;
  /** a day order: `expiryAt` is its UTC deadline and the warning window is the short one */
  dayOrder: boolean;
  /** an IOC / FOK style order (it has a kill time): its expiry is seconds away by design, never a warning */
  ephemeral: boolean;
  /** open but past its deadline / expiry: a refund is pending */
  refundable: boolean;
  token: Hex | null;
  side: 'sell' | 'buy' | null;
  /** the order's own price (sompi per whole token; null: none, e.g. a pair order). Absent in snapshots stored by an older build. */
  price?: string | null;
  /** a pair order: its quote token B (its fills are volume of A only, never a price); null / absent otherwise */
  quote?: Hex | null;
}

export const isLiveStatus = (status: string): boolean => status === 'open' || status === 'partial';

/** A stored amount (decimal string, or a number of an older snapshot) as a bigint; 0 when malformed. */
const amt = (v: string | number | null | undefined): bigint => {
  if (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) return BigInt(v);
  if (typeof v === 'string' && /^\d+$/.test(v)) return BigInt(v);
  return 0n;
};
const amtOrNull = (v: string | number | null | undefined): bigint | null => (v === null || v === undefined ? null : amt(v));

const armedOf = (v: OrderView): bigint | null => {
  const s = v.state && typeof v.state === 'object' ? (v.state as { state?: { armed?: string | number | bigint } }).state : undefined;
  if (!s || s.armed === undefined || s.armed === null) return null;
  try {
    return BigInt(s.armed);
  } catch {
    return null;
  }
};

/** One order as the diff sees it. `clock` maps an on-chain expiry (DAA) to wall-clock time; without it only a day order's deadline is known. */
export function snapshotOf(v: OrderView, clock: Clock | null): OrderSnap {
  const dayOrder = v.deadline !== null && v.deadline !== undefined;
  let expiryAt: number | null = null;
  if (dayOrder) expiryAt = Number(v.deadline);
  else if (clock && v.expiry_daa !== null && v.expiry_daa !== undefined && BigInt(v.expiry_daa) < NO_DATE_DAA) expiryAt = Number(daaToUnix(clock, BigInt(v.expiry_daa)));
  const ephemeral = v.kill_daa !== null && v.kill_daa !== undefined;
  const filled = amt(v.filled_amount);
  const remaining = amtOrNull(v.amount_left);
  const initial = amtOrNull(v.initial_amount);
  return {
    status: v.status,
    filled: filled.toString(),
    remaining: remaining === null ? null : remaining.toString(),
    total: initial !== null ? initial.toString() : remaining !== null ? (filled + remaining).toString() : null,
    armed: armedOf(v),
    auctionActive: !!v.auction && !v.auction.complete,
    expiryAt,
    dayOrder,
    ephemeral,
    refundable: isLiveStatus(v.status) && (v.expired === true || v.deadline_passed === true),
    token: v.token ?? null,
    side: v.side === 1 ? 'sell' : v.side === 2 ? 'buy' : null,
    ...(pairOfView(v) ? { price: null, quote: pairOfView(v)!.quote } : { price: v.price ?? null }),
  };
}

export const snapshotMap = (views: readonly OrderView[], clock: Clock | null): Map<Hex, OrderSnap> => new Map(views.map((v) => [v.covenant_id, snapshotOf(v, clock)]));

const inExpiryWindow = (s: OrderSnap, nowSec: number): boolean =>
  isLiveStatus(s.status) && !s.ephemeral && s.expiryAt !== null && s.expiryAt > nowSec && s.expiryAt - nowSec <= (s.dayOrder ? EXPIRY_SOON_DAY_SECONDS : EXPIRY_SOON_SECONDS);

/**
 * Events between two snapshot maps of the wallet's orders. `prev === null` is the very first poll: nothing happened "now", so only `expirySoon` for
 * orders already inside their warning window is produced. An order that appears later is compared with an "open, nothing filled" start, so an order
 * placed and filled between two polls still reports. An order that vanishes from the answer (paged out) produces nothing.
 */
export function diffOrders(prev: ReadonlyMap<Hex, OrderSnap> | null, next: ReadonlyMap<Hex, OrderSnap>, nowMs: number): NotificationEvent[] {
  const out: NotificationEvent[] = [];
  const nowSec = nowMs / 1000;
  for (const [id, n] of next) {
    const base = {
      orderId: id,
      ...(n.token ? { token: n.token } : {}),
      ...(n.side ? { side: n.side } : {}),
      at: nowMs,
      read: false,
    };
    const emit = (kind: NotifyKind, key: string | number, params: NotifyParams = {}) => out.push({ id: `${kind}:${id}:${key}`, kind, params, ...base });
    const p0 = prev ? (prev.get(id) ?? null) : null;
    if (prev === null) {
      if (inExpiryWindow(n, nowSec)) emit('expirySoon', n.expiryAt!, { at: n.expiryAt!, day: n.dayOrder ? 1 : 0 });
      continue;
    }
    const p: OrderSnap = p0 ?? { ...n, status: 'open', filled: '0', auctionActive: false, refundable: false };
    const nFilled = amt(n.filled);
    const pFilled = amt(p.filled);
    // a pair order's fill is volume of A only: the quote token rides along, no price ever
    const pair: NotifyParams = n.quote ? { quote: n.quote } : {};
    const amounts = { amount: nFilled.toString(), total: (amtOrNull(n.total) ?? nFilled + (amtOrNull(n.remaining) ?? 0n)).toString(), ...pair };

    // the chain re-organised under this order: something the wallet was told before is gone. Ordinary shallow re-orgs change nothing here and stay silent.
    if (p0) {
      const back = reorgOf(p0, n);
      if (back) out.push({ id: `reorg:${id}:${nowMs}`, kind: 'reorg', params: back, ...base });
    }

    if (n.status === 'filled' && p.status !== 'filled') emit('fill', 'filled', { amount: nFilled.toString(), ...pair });
    else if (isLiveStatus(n.status) && nFilled > pFilled) emit('partial', nFilled.toString(), amounts);
    if (p0 && p.armed !== null && p.armed === 0n && n.armed !== null && n.armed !== 0n) emit('armed', String(n.armed));
    const armedNow = (p.armed !== null && p.armed !== 0n) || (n.armed !== null && n.armed !== 0n);
    if (armedNow && (nFilled > pFilled || (n.auctionActive && !p.auctionActive))) emit('triggered', 'triggered');
    if (n.refundable && !p.refundable && !n.ephemeral) emit('expired', n.expiryAt ?? 'expired', { at: n.expiryAt ?? 0 });
    if (n.status === 'refunded' && p.status !== 'refunded') emit('refunded', 'refunded');
    if (n.status === 'killed' && p.status !== 'killed') emit('killed', 'killed', { amount: nFilled.toString(), ...pair });
    if (n.status === 'cancelled' && p.status !== 'cancelled') emit('cancelled', 'cancelled');
    if (inExpiryWindow(n, nowSec) && !(p0 && inExpiryWindow(p0, nowSec) && p0.expiryAt === n.expiryAt)) emit('expirySoon', n.expiryAt!, { at: n.expiryAt!, day: n.dayOrder ? 1 : 0 });
  }
  return out;
}

/**
 * What a chain re-organisation undid on one order between two polls, or null. Only a step BACKWARDS counts (the indexer never rolls an order back
 * otherwise): a smaller filled amount (a fill was reverted: `what: 'fill'`), or a closed order (cancelled / refunded / killed) that is live again.
 * `price` is the order's own price (the fill's price for a plain limit order; absent when the order has none).
 */
export function reorgOf(prev: OrderSnap, next: OrderSnap): NotifyParams | null {
  const nFilled = amt(next.filled);
  const pFilled = amt(prev.filled);
  const state = isLiveStatus(next.status) ? (nFilled > 0n ? 'partial' : 'open') : next.status;
  const total = (amtOrNull(next.total) ?? nFilled + (amtOrNull(next.remaining) ?? 0n)).toString();
  // a pair order has no price (its fills are volume only)
  const price = prev.quote || next.quote ? null : (prev.price ?? next.price ?? null);
  if (nFilled < pFilled) {
    return { what: 'fill', amount: (pFilled - nFilled).toString(), filled: nFilled.toString(), total, state, ...(price !== null ? { price } : {}) };
  }
  if (isLiveStatus(next.status) && (prev.status === 'cancelled' || prev.status === 'refunded' || prev.status === 'killed')) {
    return { what: prev.status, filled: nFilled.toString(), total, state };
  }
  return null;
}

/**
 * `reorg` event for a live order that the indexer no longer knows at all (its placement was in the reverted blocks). The caller must have asked for
 * the order by id: an order merely missing from a paged list is not this.
 */
export function diffVanished(prev: OrderSnap, id: Hex, nowMs: number): NotificationEvent {
  return {
    id: `reorg:${id}:${nowMs}`,
    kind: 'reorg',
    orderId: id,
    ...(prev.token ? { token: prev.token } : {}),
    ...(prev.side ? { side: prev.side } : {}),
    params: { what: 'gone', filled: amt(prev.filled).toString(), total: (amtOrNull(prev.total) ?? amt(prev.filled) + (amtOrNull(prev.remaining) ?? 0n)).toString(), state: 'gone' },
    at: nowMs,
    read: false,
  };
}

// ------------------------------------------------------------------------------------------------ incoming payments

export interface IncomingPayment {
  /** `txid:index` */
  key: string;
  /** base units (sompi for KAS) */
  amount: bigint;
  /** token covenant id; undefined = KAS */
  token?: Hex;
}

/**
 * `payment` events for P2PK outputs of the wallet that were not there at the last poll. `prev` = outpoint keys of the last poll (null: first poll, baseline
 * only). The wallet cannot tell its own change from someone else's money by the UTXO alone, so `suppress` (a caller that saw the wallet spend or an own
 * order settle in the same poll) and "a previously known outpoint disappeared" (the wallet spent inputs: the new output is its change) both drop the KAS
 * events; a token output that appears is reported unless `suppress` (order proceeds and refunds arrive as owned token UTXOs too).
 */
export function diffPayments(prev: ReadonlySet<string> | null, next: readonly IncomingPayment[], nowMs: number, suppress = false): NotificationEvent[] {
  if (prev === null || suppress) return [];
  const nextKeys = new Set(next.map((p) => p.key));
  const spent = [...prev].some((k) => !nextKeys.has(k));
  const out: NotificationEvent[] = [];
  for (const p of next) {
    if (prev.has(p.key)) continue;
    if (!p.token && spent) continue;
    out.push({ id: `payment:${p.key}`, kind: 'payment', ...(p.token ? { token: p.token } : {}), params: { amount: p.amount.toString(), ...(p.token ? { token: p.token } : {}) }, at: nowMs, read: false });
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ x402 invoices

export type InvoiceWatchState = 'unpaid' | 'pending' | 'paid' | 'expired' | 'failed';
export const INVOICE_TERMINAL: readonly string[] = ['paid', 'expired'];
export const isTerminalInvoice = (status: string | null): boolean => status !== null && INVOICE_TERMINAL.includes(status);

export interface WatchedInvoice {
  /** the invoice URL (`https://host/invoices/<64 hex>`, no query / fragment) */
  url: string;
  id: string;
  /** the merchant's reference, once the status was read */
  reference: string | null;
  /** last status seen (null: not read yet) */
  status: string | null;
  addedAt: number;
}

const HEX64 = /^[0-9a-f]{64}$/;
const LOCAL_HOSTS = new Set(['localhost', '127.0.0.1', '[::1]']);

/**
 * Validates a pasted invoice URL: `https` (or `http` on localhost only), no credentials, path `/invoices/<64 hex>`. The returned URL has no query and no
 * fragment; the watcher only ever sends a plain GET to `<url>/status` (no wallet data, no cookies).
 */
export function parseInvoiceUrl(input: string): { url: string; id: string } | null {
  let u: URL;
  try {
    u = new URL(input.trim());
  } catch {
    return null;
  }
  if (u.username || u.password) return null;
  if (!(u.protocol === 'https:' || (u.protocol === 'http:' && LOCAL_HOSTS.has(u.hostname)))) return null;
  const m = /^(.*)\/invoices\/([0-9a-fA-F]{64})\/?$/.exec(u.pathname);
  if (!m) return null;
  const id = m[2]!.toLowerCase();
  if (!HEX64.test(id)) return null;
  return { url: `${u.origin}${m[1]}/invoices/${id}`, id };
}

/** `invoice` event when the status moved to paid / expired / failed (not for unpaid -> pending, not for a status already reported). */
export function diffInvoice(w: WatchedInvoice, status: string, reference: string | null, nowMs: number): NotificationEvent | null {
  if (status === w.status) return null;
  if (status !== 'paid' && status !== 'expired' && status !== 'failed') return null;
  return { id: `invoice:${w.id}:${status}`, kind: 'invoice', params: { status, reference: reference ?? w.reference ?? '', invoice: w.id }, at: nowMs, read: false };
}

// ------------------------------------------------------------------------------------------------ settings

export interface NotifySettings {
  /** in-app notification centre and toasts (default off: opt-in) */
  enabled: boolean;
  /** browser (OS) notifications through the Notification API; needs the permission, requested from a click in Settings (default off) */
  browser: boolean;
  kinds: Record<NotifyKind, boolean>;
}

export const SETTINGS_KEY = 'kob.notify.settings';

export const defaultNotifySettings = (): NotifySettings => ({
  enabled: false,
  browser: false,
  kinds: Object.fromEntries(NOTIFY_KINDS.map((k) => [k, k !== 'cancelled'])) as Record<NotifyKind, boolean>,
});

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

export function safeStorage(): Store | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

export function loadNotifySettings(storage: Store | null = safeStorage()): NotifySettings {
  const d = defaultNotifySettings();
  try {
    const raw = storage?.getItem(SETTINGS_KEY);
    if (!raw) return d;
    const j = JSON.parse(raw) as Partial<NotifySettings> | null;
    if (!j || typeof j !== 'object') return d;
    if (typeof j.enabled === 'boolean') d.enabled = j.enabled;
    if (typeof j.browser === 'boolean') d.browser = j.browser;
    if (j.kinds && typeof j.kinds === 'object') for (const k of NOTIFY_KINDS) if (typeof j.kinds[k] === 'boolean') d.kinds[k] = j.kinds[k];
  } catch {
    /* corrupt or unreadable: defaults */
  }
  return d;
}

export function saveNotifySettings(s: NotifySettings, storage: Store | null = safeStorage()): boolean {
  try {
    if (!storage) return false;
    storage.setItem(SETTINGS_KEY, JSON.stringify(s));
    return true;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------------------------------------ store

export const MAX_NOTIFICATIONS = 200;
const MAX_SEEN_IDS = 1000;
const MAX_SNAPSHOTS = 600;
const MAX_PAYMENT_KEYS = 600;
const MAX_INVOICES = 20;

interface SnapJson extends Omit<OrderSnap, 'armed'> { armed: string | null }
interface Persisted {
  v: 1;
  /** null: no baseline yet */
  snaps: Record<string, SnapJson> | null;
  payments: string[] | null;
  items: NotificationEvent[];
  seen: string[];
  invoices: WatchedInvoice[];
}

const encodeSnap = (s: OrderSnap): SnapJson => ({ ...s, armed: s.armed === null ? null : s.armed.toString() });
const decodeSnap = (s: SnapJson): OrderSnap => ({ ...s, armed: s.armed === null || s.armed === undefined ? null : BigInt(s.armed) });

export const storeKey = (network: string, pubkey: string): string => `kob.notify.${network}.${pubkey.toLowerCase()}`;

export class NotificationStore {
  private state: Persisted = { v: 1, snaps: null, payments: null, items: [], seen: [], invoices: [] };
  private readonly listeners = new Set<() => void>();
  private readonly key: string;

  constructor(private readonly storage: Store | null, network: string, pubkey: string) {
    this.key = storeKey(network, pubkey);
    this.load();
  }

  private load(): void {
    try {
      const raw = this.storage?.getItem(this.key);
      if (!raw) return;
      const j = JSON.parse(raw) as Partial<Persisted> | null;
      if (!j || j.v !== 1) return;
      this.state = {
        v: 1,
        snaps: j.snaps && typeof j.snaps === 'object' ? j.snaps : null,
        payments: Array.isArray(j.payments) ? j.payments.filter((x) => typeof x === 'string') : null,
        items: Array.isArray(j.items) ? j.items.filter((x) => x && typeof x.id === 'string').slice(0, MAX_NOTIFICATIONS) : [],
        seen: Array.isArray(j.seen) ? j.seen.filter((x) => typeof x === 'string') : [],
        invoices: Array.isArray(j.invoices) ? j.invoices.filter((x) => x && typeof x.url === 'string' && typeof x.id === 'string') : [],
      };
    } catch {
      /* unreadable or corrupt: start empty (memory only) */
    }
  }

  private save(): void {
    try {
      this.storage?.setItem(this.key, JSON.stringify(this.state));
    } catch {
      /* storage full / blocked: the in-memory copy keeps working */
    }
    for (const l of [...this.listeners]) l();
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** The last order snapshots, or null before the first poll of this wallet. */
  snapshots(): Map<Hex, OrderSnap> | null {
    if (!this.state.snaps) return null;
    try {
      return new Map(Object.entries(this.state.snaps).map(([id, s]) => [id as Hex, decodeSnap(s)]));
    } catch {
      return null;
    }
  }

  setSnapshots(m: ReadonlyMap<Hex, OrderSnap>): void {
    // keep every live order and the newest others (insertion order of the map = the indexer's newest-first order)
    const entries = [...m.entries()];
    const live = entries.filter(([, s]) => isLiveStatus(s.status));
    const rest = entries.filter(([, s]) => !isLiveStatus(s.status)).slice(0, Math.max(0, MAX_SNAPSHOTS - live.length));
    this.state.snaps = Object.fromEntries([...live, ...rest].map(([id, s]) => [id, encodeSnap(s)]));
    this.save();
  }

  paymentKeys(): Set<string> | null {
    return this.state.payments ? new Set(this.state.payments) : null;
  }

  setPaymentKeys(keys: Iterable<string>): void {
    this.state.payments = [...keys].slice(0, MAX_PAYMENT_KEYS);
    this.save();
  }

  /** Adds events (deduplicated by id against the history and the seen list, `allowed` filters kinds); returns the ones that were new, newest first. */
  ingest(events: readonly NotificationEvent[], allowed?: (kind: NotifyKind) => boolean): NotificationEvent[] {
    const seen = new Set(this.state.seen);
    for (const e of this.state.items) seen.add(e.id);
    const fresh: NotificationEvent[] = [];
    for (const e of events) {
      if (allowed && !allowed(e.kind)) continue;
      if (seen.has(e.id)) continue;
      seen.add(e.id);
      fresh.push({ ...e, read: false });
    }
    if (!fresh.length) return [];
    fresh.reverse();
    this.state.items = [...fresh, ...this.state.items].slice(0, MAX_NOTIFICATIONS);
    this.state.seen = [...seen].slice(-MAX_SEEN_IDS);
    this.save();
    return fresh;
  }

  items(): readonly NotificationEvent[] {
    return this.state.items;
  }

  unread(): number {
    return this.state.items.filter((e) => !e.read).length;
  }

  markAllRead(): void {
    if (!this.state.items.some((e) => !e.read)) return;
    this.state.items = this.state.items.map((e) => (e.read ? e : { ...e, read: true }));
    this.save();
  }

  /** Empties the list (ids stay in the seen list: a cleared notice does not come back). */
  clear(): void {
    if (!this.state.items.length) return;
    this.state.items = [];
    this.save();
  }

  invoices(): readonly WatchedInvoice[] {
    return this.state.invoices;
  }

  /** false: already watched or too many. */
  addInvoice(w: WatchedInvoice): boolean {
    if (this.state.invoices.some((x) => x.id === w.id) || this.state.invoices.length >= MAX_INVOICES) return false;
    this.state.invoices = [...this.state.invoices, w];
    this.save();
    return true;
  }

  updateInvoice(id: string, patch: Partial<Pick<WatchedInvoice, 'status' | 'reference'>>): void {
    this.state.invoices = this.state.invoices.map((x) => (x.id === id ? { ...x, ...patch } : x));
    this.save();
  }

  removeInvoice(id: string): void {
    this.state.invoices = this.state.invoices.filter((x) => x.id !== id);
    this.save();
  }
}
