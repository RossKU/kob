// One notification poll of a wallet, and one of its watched invoices (no timers, no DOM: the provider schedules them).
//
// Orders: the indexer's live orders of the maker (every page of `status=active`) plus the newest page of the maker's unfiltered history, so an order
// that just left the active list (filled, killed, refunded) is still seen with its final status. They become snapshots, are diffed against the stored
// ones and stored again. Any source that fails leaves the stored state untouched (a down indexer never reads as "everything vanished").
// Payments: P2PK KAS UTXOs from the node (`utxos.fundingFor`) and `owned` token UTXOs from the indexer; see `diffPayments` for the change heuristic.
import { readClock } from '../../app/env';
import type { Services } from '../../app/services';
import type { OrderView } from '../../data/indexer-types';
import { diffInvoice, diffOrders, diffPayments, diffVanished, isLiveStatus, isTerminalInvoice, snapshotMap, type IncomingPayment, type NotificationEvent, type NotificationStore } from '../../kob/notifications';
import type { Clock } from '../../kob/plan-types';
import type { Hex } from '../../kob/types';

export const ORDERS_POLL_MS = 15_000;
export const INVOICE_POLL_MS = 30_000;
/** The newest history page looked at next to the live orders (new fills of recently placed orders). */
const RECENT_LIMIT = 100;
/** kinds that mean "an own order settled": the proceeds / refund that arrive in the same poll are not an outside payment */
const SETTLES: ReadonlySet<string> = new Set(['fill', 'partial', 'triggered', 'expired', 'refunded', 'killed', 'cancelled', 'reorg']);
/** At most this many live orders that fell out of the lists are asked for by id in one poll (a re-org rarely takes more). */
const MAX_LOOKUPS = 20;

async function pollOrders(s: Services, store: NotificationStore, pubkey: Hex, nowMs: number, signal: AbortSignal | undefined): Promise<NotificationEvent[]> {
  if (!s.indexer) return [];
  const sig = signal ? { signal } : {};
  const [active, recent, clock] = await Promise.all([
    s.indexer.allOrders({ maker: pubkey, status: 'active', limit: 200 }, { ...sig, maxPages: Number.MAX_SAFE_INTEGER }),
    s.indexer.orders({ maker: pubkey, limit: RECENT_LIMIT }, sig).then((p) => p.items),
    readClock(s).catch((): Clock | null => null),
  ]);
  const byId = new Map<Hex, OrderView>();
  for (const v of recent) byId.set(v.covenant_id, v);
  for (const v of active) byId.set(v.covenant_id, v);
  const prev = store.snapshots();
  // A live order that is in neither list is either paged out of the recent window or gone from the chain (a re-org took its placement): ask the
  // indexer for it by id. Only a definite "not found" counts as gone; an error leaves the stored snapshot alone.
  const gone: NotificationEvent[] = [];
  const dropped = new Set<Hex>();
  if (prev) {
    const missing = [...prev].filter(([id, snap]) => isLiveStatus(snap.status) && !byId.has(id)).slice(0, MAX_LOOKUPS);
    await Promise.all(
      missing.map(async ([id, snap]) => {
        try {
          const v = await s.indexer!.order(id, sig);
          if (v) byId.set(id, v);
          else {
            gone.push(diffVanished(snap, id, nowMs));
            dropped.add(id);
          }
        } catch {
          /* unreachable: try again next poll */
        }
      }),
    );
  }
  const next = snapshotMap([...byId.values()], clock);
  const events = [...diffOrders(prev, next, nowMs), ...gone];
  // keep what the answer did not cover (an older order paged out of the recent window) so it is not mistaken for new if it reappears
  if (prev) for (const [id, snap] of prev) if (!next.has(id) && !dropped.has(id)) next.set(id, snap);
  store.setSnapshots(next);
  return events;
}

async function pollPayments(s: Services, store: NotificationStore, pubkey: Hex, nowMs: number, settled: boolean, signal: AbortSignal | undefined): Promise<NotificationEvent[]> {
  const [kas, tokens] = await Promise.all([
    s.utxos.fundingFor(pubkey),
    s.indexer ? s.indexer.tokenUtxos({ owner: pubkey, spent: false, maxPages: 3 }, signal ? { signal } : {}) : Promise.resolve(null),
  ]);
  const next: IncomingPayment[] = kas.map((u) => ({ key: `${u.transactionId}:${u.index}`, amount: BigInt(u.amount) }));
  for (const u of tokens ?? []) if (u.role === 'owned' && !u.spent) next.push({ key: `${u.txid}:${u.index}`, amount: BigInt(u.amount), token: u.token });
  const events = diffPayments(store.paymentKeys(), next, nowMs, settled);
  store.setPaymentKeys(next.map((p) => p.key));
  return events;
}

/**
 * Polls the orders and the incoming payments of one wallet; returns every event found (not yet stored: the caller `ingest`s them). The two sources fail
 * independently: `errors` counts the failed ones.
 */
export async function pollWallet(s: Services, store: NotificationStore, pubkey: Hex, nowMs: number, signal?: AbortSignal): Promise<{ events: NotificationEvent[]; errors: number }> {
  let errors = 0;
  const orderEvents = await pollOrders(s, store, pubkey, nowMs, signal).catch(() => (errors++, [] as NotificationEvent[]));
  const settled = orderEvents.some((e) => SETTLES.has(e.kind));
  const payEvents = await pollPayments(s, store, pubkey, nowMs, settled, signal).catch(() => (errors++, [] as NotificationEvent[]));
  return { events: [...orderEvents, ...payEvents], errors };
}

export type FetchFn = (url: string, init?: RequestInit) => Promise<Response>;

/** Reads `<url>/status` of every watched invoice that is not terminal (plain GET, nothing of the wallet in it); stores the new statuses and returns the events. */
export async function pollInvoices(store: NotificationStore, nowMs: number, fetchFn: FetchFn = (u, i) => fetch(u, i)): Promise<NotificationEvent[]> {
  const out: NotificationEvent[] = [];
  for (const w of store.invoices()) {
    if (isTerminalInvoice(w.status)) continue;
    try {
      const res = await fetchFn(`${w.url}/status`, { method: 'GET', headers: { accept: 'application/json' }, redirect: 'error', credentials: 'omit', cache: 'no-store', signal: AbortSignal.timeout(10_000) });
      if (!res.ok) continue;
      const j = (await res.json()) as { status?: unknown; reference?: unknown } | null;
      if (!j || typeof j.status !== 'string' || j.status.length > 20) continue;
      const reference = typeof j.reference === 'string' ? j.reference.slice(0, 80) : null;
      const ev = diffInvoice(w, j.status, reference, nowMs);
      if (ev) out.push(ev);
      store.updateInvoice(w.id, { status: j.status, ...(reference !== null ? { reference } : {}) });
    } catch {
      /* unreachable / not JSON: try again next round */
    }
  }
  return out;
}
