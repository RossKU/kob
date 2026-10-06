import { describe, expect, it } from 'vitest';
import type { OrderView } from '../data/indexer-types';
import {
  MAX_NOTIFICATIONS, NOTIFY_KINDS, NotificationStore, SETTINGS_KEY, defaultNotifySettings, diffInvoice, diffOrders, diffPayments, diffVanished, loadNotifySettings, parseInvoiceUrl,
  reorgOf, saveNotifySettings, snapshotMap, snapshotOf, storeKey, type NotificationEvent, type NotifyKind, type OrderSnap, type WatchedInvoice,
} from './notifications';
import type { Clock } from './plan-types';

const ID = 'aa'.repeat(32);
const ID2 = 'bb'.repeat(32);
const TOKEN = 'cc'.repeat(32);
const NOW = 1_800_000_000_000;
const clock: Clock = { daa: 1_000_000n, unixSeconds: BigInt(NOW / 1000), rateMilli: 10_000 };

const snap = (o: Partial<OrderSnap> = {}): OrderSnap => ({
  status: 'open', filled: '0', remaining: '10', total: '10', armed: null, auctionActive: false, expiryAt: null, dayOrder: false, ephemeral: false, refundable: false, token: TOKEN, side: 'sell', ...o,
});
const m = (...e: [string, OrderSnap][]) => new Map(e);
const kinds = (ev: { kind: string }[]) => ev.map((e) => e.kind);

const view = (o: Partial<OrderView> = {}): OrderView => ({
  covenant_id: ID, contract: 'KobAsk', template_hash: '00', family: 1, side: 1, maker: null, token: TOKEN, token_template_hash: null, scale: 1000, min_fill: '1000', price: '1', tip: null, tif: 0,
  expiry_daa: null, active_from: null, in_book: true, budget_rate: null, reserve: null, initial_amount: '10000', listed: true, unlisted_reason: null, origin: 'x', parent: null,
  genesis: { txid: null, out: null, block_seq: 0, daa: 1, confirmations: 1, settled: true }, status: 'open', filled_amount: '0', amount_left: '10000', amount_estimated: false, current: null,
  state_known: true, last_block: 0, last_daa: 0, confirmations: 1, settled: true, expired: false, ...o,
}) as OrderView;

class MemStorage {
  data = new Map<string, string>();
  getItem(k: string) { return this.data.get(k) ?? null; }
  setItem(k: string, v: string) { this.data.set(k, v); }
  removeItem(k: string) { this.data.delete(k); }
}
const throwing = {
  getItem: (): string | null => { throw new Error('blocked'); },
  setItem: (): void => { throw new Error('blocked'); },
  removeItem: (): void => { throw new Error('blocked'); },
};

describe('snapshotOf', () => {
  it('reads status, amounts, armed state, auction and expiry', () => {
    const s = snapshotOf(view({ filled_amount: '3000', amount_left: '7000', state: { kind: 'KobCondAsk', state: { armed: '5' } } as never, auction: { kind: 'stop', complete: false } as never, expiry_daa: 1_000_000 + 600_000 }), clock);
    expect(s).toMatchObject({ filled: '3000', remaining: '7000', total: '10000', armed: 5n, auctionActive: true, side: 'sell', dayOrder: false });
    expect(s.expiryAt).toBe(NOW / 1000 + 60_000);
  });
  it('uses the day-order deadline, ignores no-date sentinels and marks kill-time orders ephemeral', () => {
    expect(snapshotOf(view({ deadline: NOW / 1000 + 100 }), null)).toMatchObject({ dayOrder: true, expiryAt: NOW / 1000 + 100 });
    expect(snapshotOf(view({ expiry_daa: Number(1n << 62n) }), clock).expiryAt).toBeNull();
    expect(snapshotOf(view({ kill_daa: 5 }), clock).ephemeral).toBe(true);
    expect(snapshotOf(view({ expired: true }), clock).refundable).toBe(true);
    expect(snapshotOf(view({ expired: true, status: 'filled' }), clock).refundable).toBe(false);
  });
  it('snapshotMap keys by covenant id', () => {
    expect([...snapshotMap([view(), view({ covenant_id: ID2 })], clock).keys()]).toEqual([ID, ID2]);
  });
});

describe('diffOrders', () => {
  it('first poll records the baseline: no fill / partial history', () => {
    expect(diffOrders(null, m([ID, snap({ status: 'filled', filled: '10', remaining: '0' })], [ID2, snap({ filled: '4' })]), NOW)).toEqual([]);
  });
  it('first poll still warns about an order already close to expiry (once)', () => {
    const ev = diffOrders(null, m([ID, snap({ expiryAt: NOW / 1000 + 3600 })]), NOW);
    expect(kinds(ev)).toEqual(['expirySoon']);
    expect(ev[0]!.id).toBe(`expirySoon:${ID}:${NOW / 1000 + 3600}`);
  });
  it('fill: status became filled', () => {
    const ev = diffOrders(m([ID, snap()]), m([ID, snap({ status: 'filled', filled: '10', remaining: '0' })]), NOW);
    expect(kinds(ev)).toEqual(['fill']);
    expect(ev[0]).toMatchObject({ id: `fill:${ID}:filled`, orderId: ID, token: TOKEN, side: 'sell', read: false, at: NOW });
  });
  it('partial: the filled amount grew but the order is still live', () => {
    const ev = diffOrders(m([ID, snap({ filled: '2' })]), m([ID, snap({ status: 'partial', filled: '5', remaining: '5' })]), NOW);
    expect(kinds(ev)).toEqual(['partial']);
    expect(ev[0]).toMatchObject({ id: `partial:${ID}:5`, params: { amount: '5', total: '10' } });
  });
  it('a snapshot stored by an older build (numbers, not decimal strings) still compares', () => {
    const old = { ...snap(), filled: 2, remaining: 8, total: 10 } as unknown as OrderSnap;
    const ev = diffOrders(m([ID, old]), m([ID, snap({ status: 'partial', filled: '5', remaining: '5' })]), NOW);
    expect(ev).toEqual([expect.objectContaining({ kind: 'partial', params: { amount: '5', total: '10' } })]);
  });
  it('nothing changed: no events; an order placed and filled between polls reports', () => {
    expect(diffOrders(m([ID, snap()]), m([ID, snap()]), NOW)).toEqual([]);
    expect(kinds(diffOrders(m(), m([ID, snap({ status: 'filled', filled: '10', remaining: '0' })]), NOW))).toEqual(['fill']);
  });
  it('armed: 0 -> nonzero; triggered: an armed stop starts filling or its auction becomes active', () => {
    expect(kinds(diffOrders(m([ID, snap({ armed: 0n })]), m([ID, snap({ armed: 7n })]), NOW))).toEqual(['armed']);
    expect(kinds(diffOrders(m([ID, snap({ armed: 7n })]), m([ID, snap({ armed: 7n, auctionActive: true })]), NOW))).toEqual(['triggered']);
    const ev = diffOrders(m([ID, snap({ armed: 7n })]), m([ID, snap({ armed: 7n, status: 'partial', filled: '1', remaining: '9' })]), NOW);
    expect(kinds(ev).sort()).toEqual(['partial', 'triggered']);
    // an unarmed order that fills is not a trigger
    expect(kinds(diffOrders(m([ID, snap({ armed: 0n })]), m([ID, snap({ armed: 0n, status: 'partial', filled: '1' })]), NOW))).toEqual(['partial']);
  });
  it('expirySoon: 24 h for GTC / GTD, 1 h for day orders, once per expiry value, never for kill-time orders', () => {
    const at = NOW / 1000;
    expect(kinds(diffOrders(m([ID, snap({ expiryAt: at + 10 * 3600 })]), m([ID, snap({ expiryAt: at + 10 * 3600 })]), NOW))).toEqual([]); // was already in the window
    expect(kinds(diffOrders(m([ID, snap({ expiryAt: at + 30 * 3600 })]), m([ID, snap({ expiryAt: at + 23 * 3600 })]), NOW))).toEqual(['expirySoon']);
    expect(kinds(diffOrders(m([ID, snap({ expiryAt: at + 5000, dayOrder: true })]), m([ID, snap({ expiryAt: at + 5000, dayOrder: true })]), NOW))).toEqual([]);
    expect(kinds(diffOrders(m([ID, snap({ expiryAt: at + 5000, dayOrder: true })]), m([ID, snap({ expiryAt: at + 3000, dayOrder: true })]), NOW))).toEqual(['expirySoon']);
    expect(diffOrders(m([ID, snap()]), m([ID, snap({ expiryAt: at + 60, ephemeral: true })]), NOW)).toEqual([]);
    expect(diffOrders(m([ID, snap()]), m([ID, snap({ expiryAt: at - 5 })]), NOW)).toEqual([]); // already past: that is `expired`'s business
  });
  it('expired / refunded / killed / cancelled', () => {
    expect(kinds(diffOrders(m([ID, snap()]), m([ID, snap({ refundable: true, expiryAt: 5 })]), NOW))).toEqual(['expired']);
    expect(kinds(diffOrders(m([ID, snap({ refundable: true })]), m([ID, snap({ status: 'refunded' })]), NOW))).toEqual(['refunded']);
    expect(kinds(diffOrders(m([ID, snap()]), m([ID, snap({ status: 'killed', filled: '2' })]), NOW))).toEqual(['killed']);
    expect(kinds(diffOrders(m([ID, snap()]), m([ID, snap({ status: 'cancelled' })]), NOW))).toEqual(['cancelled']);
  });
});

describe('chain re-organisations (only what changes an own order is reported)', () => {
  it('an unchanged order, or one that only moved forward, yields no reorg event', () => {
    expect(diffOrders(m([ID, snap({ price: '25050' })]), m([ID, snap({ price: '25050' })]), NOW)).toEqual([]);
    expect(kinds(diffOrders(m([ID, snap()]), m([ID, snap({ status: 'partial', filled: '3' })]), NOW))).not.toContain('reorg');
  });
  it('a reverted partial fill names the amount, the price and the state the order is in now', () => {
    const ev = diffOrders(m([ID, snap({ status: 'partial', filled: '3', remaining: '7', price: '25050' })]), m([ID, snap({ status: 'open', filled: '0', remaining: '10', price: '25050' })]), NOW);
    expect(ev).toHaveLength(1);
    expect(ev[0]).toMatchObject({ kind: 'reorg', orderId: ID, token: TOKEN, side: 'sell', read: false, params: { what: 'fill', amount: '3', filled: '0', total: '10', state: 'open', price: '25050' } });
    expect(ev[0]!.id.startsWith(`reorg:${ID}:`)).toBe(true);
  });
  it('a reverted fill that closed the order: it is live again, partly filled when earlier fills stand', () => {
    const ev = diffOrders(m([ID, snap({ status: 'filled', filled: '10', remaining: '0' })]), m([ID, snap({ status: 'partial', filled: '6', remaining: '4' })]), NOW);
    expect(ev[0]).toMatchObject({ kind: 'reorg', params: { what: 'fill', amount: '4', filled: '6', total: '10', state: 'partial' } });
    expect(ev[0]!.params).not.toHaveProperty('price'); // snapshot without a price (older build / cross limit): the text just omits it
  });
  it('a reverted cancel / refund / kill: the order is live again', () => {
    for (const status of ['cancelled', 'refunded', 'killed']) {
      const ev = diffOrders(m([ID, snap({ status })]), m([ID, snap()]), NOW);
      expect(ev).toHaveLength(1);
      expect(ev[0]).toMatchObject({ kind: 'reorg', params: { what: status, state: 'open' } });
    }
  });
  it('reorgOf: null when nothing went backwards', () => {
    expect(reorgOf(snap({ filled: '2' }), snap({ filled: '2' }))).toBeNull();
    expect(reorgOf(snap({ status: 'cancelled' }), snap({ status: 'cancelled' }))).toBeNull();
  });
  it('an order the indexer no longer knows gives a "gone" event', () => {
    const e = diffVanished(snap({ status: 'partial', filled: '2', remaining: '8' }), ID, NOW);
    expect(e).toMatchObject({ kind: 'reorg', orderId: ID, token: TOKEN, side: 'sell', params: { what: 'gone', filled: '2', total: '10' } });
  });
  it('kind is a notification setting, on by default', () => {
    expect(NOTIFY_KINDS).toContain('reorg');
    expect(defaultNotifySettings().kinds.reorg).toBe(true);
  });
  it('the store drops a repeated id but keeps a later, separate reorg', () => {
    const st = new NotificationStore(new MemStorage(), 'testnet-10', 'ab'.repeat(32));
    const a = diffOrders(m([ID, snap({ filled: '3', status: 'partial' })]), m([ID, snap()]), NOW);
    expect(st.ingest(a)).toHaveLength(1);
    expect(st.ingest(a)).toHaveLength(0);
    expect(st.ingest(diffOrders(m([ID, snap({ filled: '3', status: 'partial' })]), m([ID, snap()]), NOW + 60_000))).toHaveLength(1);
  });
});

describe('diffPayments', () => {
  const pay = (key: string, amount = 5n, token?: string) => ({ key, amount, ...(token ? { token } : {}) });
  it('baseline on the first poll, then one event per new outpoint', () => {
    expect(diffPayments(null, [pay('a:0')], NOW)).toEqual([]);
    const ev = diffPayments(new Set(['a:0']), [pay('a:0'), pay('b:1', 7n)], NOW);
    expect(ev).toHaveLength(1);
    expect(ev[0]).toMatchObject({ id: 'payment:b:1', kind: 'payment', params: { amount: '7' } });
  });
  it('drops KAS change (an input vanished) and everything when suppressed; keeps token receipts', () => {
    expect(diffPayments(new Set(['a:0']), [pay('c:0')], NOW)).toEqual([]);
    expect(diffPayments(new Set(['a:0']), [pay('a:0'), pay('c:0')], NOW, true)).toEqual([]);
    const tok = diffPayments(new Set(['a:0']), [pay('c:0', 3n, TOKEN)], NOW);
    expect(tok[0]).toMatchObject({ token: TOKEN, params: { amount: '3', token: TOKEN } });
  });
});

describe('invoices', () => {
  const H = 'ab'.repeat(32);
  it('validates the URL: https (http only on localhost), 64 hex id, no credentials', () => {
    expect(parseInvoiceUrl(` https://pay.example.com/invoices/${H} `)).toEqual({ url: `https://pay.example.com/invoices/${H}`, id: H });
    expect(parseInvoiceUrl(`https://pay.example.com/x/invoices/${H.toUpperCase()}/?a=1#f`)).toEqual({ url: `https://pay.example.com/x/invoices/${H}`, id: H });
    expect(parseInvoiceUrl(`http://localhost:8080/invoices/${H}`)?.id).toBe(H);
    expect(parseInvoiceUrl(`http://127.0.0.1/invoices/${H}`)?.id).toBe(H);
    expect(parseInvoiceUrl(`http://pay.example.com/invoices/${H}`)).toBeNull();
    expect(parseInvoiceUrl(`https://u:p@pay.example.com/invoices/${H}`)).toBeNull();
    expect(parseInvoiceUrl(`https://pay.example.com/invoices/${H}/status`)).toBeNull();
    expect(parseInvoiceUrl('https://pay.example.com/invoices/abc')).toBeNull();
    expect(parseInvoiceUrl('javascript:alert(1)')).toBeNull();
    expect(parseInvoiceUrl('not a url')).toBeNull();
  });
  it('emits on paid / expired / failed once per change', () => {
    const w: WatchedInvoice = { url: 'u', id: H, reference: 'order-1', status: 'unpaid', addedAt: 0 };
    expect(diffInvoice(w, 'unpaid', null, NOW)).toBeNull();
    expect(diffInvoice(w, 'pending', null, NOW)).toBeNull();
    expect(diffInvoice(w, 'paid', null, NOW)).toMatchObject({ id: `invoice:${H}:paid`, kind: 'invoice', params: { status: 'paid', reference: 'order-1' } });
    expect(diffInvoice({ ...w, status: 'paid' }, 'paid', null, NOW)).toBeNull();
  });
});

describe('settings', () => {
  it('defaults: opt-in off, browser off, all kinds on except cancelled', () => {
    const d = defaultNotifySettings();
    expect(d.enabled).toBe(false);
    expect(d.browser).toBe(false);
    for (const k of NOTIFY_KINDS) expect(d.kinds[k]).toBe(k !== 'cancelled');
  });
  it('round-trips, merges partial / corrupt data with defaults, survives throwing storage', () => {
    const st = new MemStorage();
    const s = defaultNotifySettings();
    s.enabled = true;
    s.kinds.fill = false;
    expect(saveNotifySettings(s, st)).toBe(true);
    expect(loadNotifySettings(st)).toEqual(s);
    st.setItem(SETTINGS_KEY, '{"enabled":true,"kinds":{"fill":"x","payment":false}}');
    const l = loadNotifySettings(st);
    expect(l.enabled).toBe(true);
    expect(l.kinds.fill).toBe(true);
    expect(l.kinds.payment).toBe(false);
    st.setItem(SETTINGS_KEY, '{oops');
    expect(loadNotifySettings(st)).toEqual(defaultNotifySettings());
    expect(saveNotifySettings(s, throwing)).toBe(false);
    expect(loadNotifySettings(throwing)).toEqual(defaultNotifySettings());
  });
});

describe('NotificationStore', () => {
  const ev = (id: string, kind: NotifyKind = 'fill'): NotificationEvent => ({ id, kind, params: {}, at: 1, read: false });
  it('dedups by id, newest first, caps at 200', () => {
    const s = new NotificationStore(null, 'mainnet', 'pk');
    expect(s.ingest([ev('a'), ev('b')]).map((e) => e.id)).toEqual(['b', 'a']);
    expect(s.ingest([ev('a'), ev('c')]).map((e) => e.id)).toEqual(['c']);
    expect(s.items().map((e) => e.id)).toEqual(['c', 'b', 'a']);
    s.ingest(Array.from({ length: 250 }, (_, i) => ev(`x${i}`)));
    expect(s.items()).toHaveLength(MAX_NOTIFICATIONS);
    expect(s.items()[0]!.id).toBe('x249');
  });
  it('filters kinds, tracks unread, mark-read and clear (cleared ids do not return)', () => {
    const s = new NotificationStore(null, 'mainnet', 'pk');
    s.ingest([ev('a'), ev('b', 'cancelled')], (k) => k !== 'cancelled');
    expect(s.unread()).toBe(1);
    s.markAllRead();
    expect(s.unread()).toBe(0);
    s.clear();
    expect(s.items()).toEqual([]);
    expect(s.ingest([ev('a')])).toEqual([]);
  });
  it('persists per network + wallet, including snapshots (bigint armed) and watched invoices', () => {
    const st = new MemStorage();
    const a = new NotificationStore(st, 'mainnet', 'PK');
    expect(a.snapshots()).toBeNull();
    a.setSnapshots(m([ID, snap({ armed: 9n })]));
    a.setPaymentKeys(['t:0']);
    a.ingest([ev('a')]);
    expect(a.addInvoice({ url: 'u', id: 'i', reference: null, status: null, addedAt: 1 })).toBe(true);
    expect(a.addInvoice({ url: 'u', id: 'i', reference: null, status: null, addedAt: 1 })).toBe(false);
    a.updateInvoice('i', { status: 'pending' });
    expect(st.data.has(storeKey('mainnet', 'pk'))).toBe(true);
    const b = new NotificationStore(st, 'mainnet', 'pk');
    expect(b.snapshots()!.get(ID)!.armed).toBe(9n);
    expect([...b.paymentKeys()!]).toEqual(['t:0']);
    expect(b.items().map((e) => e.id)).toEqual(['a']);
    expect(b.invoices()[0]!.status).toBe('pending');
    expect(b.ingest([ev('a')])).toEqual([]);
    expect(new NotificationStore(st, 'testnet-10', 'pk').snapshots()).toBeNull();
    b.removeInvoice('i');
    expect(b.invoices()).toEqual([]);
  });
  it('a fill while the tab was closed shows on the next load', () => {
    const st = new MemStorage();
    const a = new NotificationStore(st, 'mainnet', 'pk');
    a.setSnapshots(m([ID, snap()]));
    const b = new NotificationStore(st, 'mainnet', 'pk');
    const next = m([ID, snap({ status: 'filled', filled: '10', remaining: '0' })]);
    expect(kinds(b.ingest(diffOrders(b.snapshots(), next, NOW)))).toEqual(['fill']);
  });
  it('works in memory when localStorage throws', () => {
    const s = new NotificationStore(throwing, 'mainnet', 'pk');
    expect(s.ingest([ev('a')])).toHaveLength(1);
    s.setSnapshots(m([ID, snap()]));
    expect(s.snapshots()!.size).toBe(1);
    expect(s.unread()).toBe(1);
  });
  it('notifies subscribers', () => {
    const s = new NotificationStore(null, 'mainnet', 'pk');
    let n = 0;
    const off = s.subscribe(() => n++);
    s.ingest([ev('a')]);
    off();
    s.ingest([ev('b')]);
    expect(n).toBe(1);
  });
});
