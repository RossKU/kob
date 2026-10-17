// Loads "My orders": the indexer's orders + strays for the wallet, the wallet's placement records, and the node's verdict on every record the
// indexer does not know (`resolveRecord`). A fresh placement or an indexer outage therefore still shows the order. Each source fails on its own.
//
// The node never holds the list back: `loadOrders` answers once the indexer and the record store have (the node's clock only if it came within
// CLOCK_WAIT_MS), with the record verdicts this session already has; the others are looked up after it (each bounded by RESOLVE_TIMEOUT_MS) and
// the list is handed over again with them (`onUpdate`). A verdict that the order is gone is kept for the session; a live one for LIVE_CACHE_MS.
import { readClock } from '../../app/env';
import type { Services } from '../../app/services';
import type { IndexerApi } from '../../data/indexer';
import type { EventView, OrderView, StrayView } from '../../data/indexer-types';
import { spkStringToAddress } from '../../data/kaspa-sdk';
import type { OrderSnapshot } from '../../kob/cancel';
import { snapshotFromRecord } from '../../kob/cancel';
import type { Clock } from '../../kob/plan-types';
import { groupPositions, type Position } from '../../kob/positions';
import { recordFromView, refreshFromResolved, refreshFromView, resolveRecord, type PlacementRecord, type RecordStore, type ResolvedRecord } from '../../kob/records';
import type { Hex } from '../../kob/types';
import { toError } from '../kit/hooks';
import { buildEntries, recordsToResolve, type OrderEntry } from './orders-model';

export interface OrdersData {
  entries: OrderEntry[];
  /** the indexer's orders of the wallet (all statuses) */
  views: OrderView[];
  /** entries/exits grouped for display (indexer views only) */
  positions: Position[];
  strays: StrayView[];
  /** the maker-wide stray list could not be read (a cancel from the record path then says strays are unknown instead of losing them silently) */
  straysError: Error | null;
  clock: Clock | null;
  /** error of the indexer queries (null: fine or not configured) */
  indexerError: Error | null;
  nodeError: Error | null;
  records: PlacementRecord[];
  /** live orders known only from records resolved on the node */
  snapshots: OrderSnapshot[];
  /** records whose node lookup failed (their status is unknown) */
  resolveFailures: number;
}

/** Runs `fn` over `items` with at most `n` in flight; results keep their order. */
export async function pool<T, R>(items: readonly T[], n: number, fn: (x: T) => Promise<R>): Promise<R[]> {
  const out = new Array<R>(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(n, items.length) }, worker));
  return out;
}

export const spkToAddressFor = (s: Pick<Services, 'sdk' | 'config'>) => (spk: string): string => spkStringToAddress(s.sdk, spk, s.config.network);

/** How long one record's node lookup may take before it counts as failed (a slow node only delays the background merge). */
export const RESOLVE_TIMEOUT_MS = 5_000;
/** How long the list waits for the node's clock before it shows without it (it follows with the record verdicts). */
export const CLOCK_WAIT_MS = 800;
/** How long a verdict that a record's order is live is reused (a gone order's verdict is kept for the session). */
export const LIVE_CACHE_MS = 15_000;

const within = <T>(p: Promise<T>, ms: number): Promise<T | null> =>
  new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), ms);
    p.then(
      (v) => (clearTimeout(timer), resolve(v)),
      () => (clearTimeout(timer), resolve(null)),
    );
  });

/** Resolves records on the node (4 at a time, each within RESOLVE_TIMEOUT_MS). A failed or timed-out lookup yields `null` for that record. */
export async function resolveRecords(s: Pick<Services, 'kob' | 'node' | 'sdk' | 'config'>, records: readonly PlacementRecord[], spkToAddress?: (spk: string) => string): Promise<Map<Hex, ResolvedRecord | null>> {
  const spk = spkToAddress ?? spkToAddressFor(s);
  const results = await pool(records, 4, (r) => within(resolveRecord(s.kob, s.node, r, spk), RESOLVE_TIMEOUT_MS));
  return new Map(records.map((r, i) => [r.covenantId, results[i]]));
}

/**
 * The verdicts of this session for one node, by covenant id and the record's state (a record that learns a new state is looked up again): a
 * gone order's for the session, a live one's for LIVE_CACHE_MS; a failed lookup is not kept.
 */
type Verdicts = Map<string, { res: ResolvedRecord; at: number }>;
const sessions = new WeakMap<object, Verdicts>();
const verdictsOf = (node: object): Verdicts => {
  let v = sessions.get(node);
  if (!v) sessions.set(node, (v = new Map()));
  return v;
};
const verdictKey = (r: PlacementRecord): string => `${r.covenantId}:${r.state}`;
const cachedVerdict = (verdicts: Verdicts, r: PlacementRecord, now: number): ResolvedRecord | null => {
  const v = verdicts.get(verdictKey(r));
  if (!v) return null;
  if (v.res.status === 'live' && now - v.at > LIVE_CACHE_MS) return null;
  return v.res;
};

export interface LoadOrdersOptions {
  signal?: AbortSignal;
  /**
   * the list again once the record verdicts the first answer did not have came from the node (and its clock, if it was late); not called when
   * there was nothing to add. Without it `loadOrders` waits for them (tests, callers that need the node's view).
   */
  onUpdate?(data: OrdersData): void;
  /** script public key -> address (tests); default: the SDK's, for the configured network */
  spkToAddress?: (spk: string) => string;
}

/** Pages of the unfiltered (newest first) order history loaded next to every live order: enough for "done" rows, bounded. */
export const HISTORY_PAGES = 5;
/** A cancel whose order is still seen at the outpoint it spent this long after submission did not happen (rejected, reorged). */
export const CANCEL_PENDING_SECONDS = 3600n;

/**
 * Every live order of the maker (`status=active`, every page: the indexer pages newest first, and the oldest live order of a busy repeat, its
 * ENTRY, must never fall off the list) plus a bounded window of recent history, merged (the active answer wins).
 */
export async function loadMakerOrders(indexer: Pick<IndexerApi, 'allOrders'>, maker: Hex, sig: { signal?: AbortSignal }): Promise<OrderView[]> {
  const [active, recent] = await Promise.all([
    indexer.allOrders({ maker, status: 'active', limit: 200 }, { ...sig, maxPages: Number.MAX_SAFE_INTEGER }),
    indexer.allOrders({ maker, limit: 200 }, { ...sig, maxPages: HISTORY_PAGES }),
  ]);
  const byId = new Map<Hex, OrderView>();
  for (const v of recent) byId.set(v.covenant_id, v);
  for (const v of active) byId.set(v.covenant_id, v);
  return [...byId.values()];
}

const TERMINAL_VIEW = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);
const isLiveView = (v: OrderView): boolean => v.status === 'open' || v.status === 'partial';

/**
 * Keeps the record store current (C5-03 / C5-06): every own live order the indexer proves gets a record (if-done exits have none: they are
 * created by their entry's fill), every record learns the last proven state, and a record marked `cancelling` is dropped once that is final
 * (the order is terminal / spent) or un-marked when the order lives on at another outpoint (a fill won the race) or the cancel never landed.
 * Returns the records as they are now. Store failures are ignored: the store is a convenience.
 */
export async function syncRecords(
  s: Pick<Services, 'kob' | 'config'>,
  pubkey: Hex,
  store: RecordStore,
  records: readonly PlacementRecord[],
  views: readonly OrderView[],
  resolved: ReadonlyMap<Hex, ResolvedRecord>,
  nowUnix: bigint | null,
): Promise<PlacementRecord[]> {
  const byId = new Map(records.map((r) => [r.covenantId, r]));
  const viewById = new Map(views.map((v) => [v.covenant_id, v]));
  const write = async (r: PlacementRecord) => {
    byId.set(r.covenantId, r);
    try {
      await store.put(r);
    } catch {
      /* memory copy only */
    }
  };
  const drop = async (id: Hex) => {
    byId.delete(id);
    try {
      await store.remove(id);
    } catch {
      /* ignore */
    }
  };
  for (const v of views) {
    if (!isLiveView(v) || byId.has(v.covenant_id)) continue;
    const r = recordFromView(s.kob, v, pubkey, s.config.network);
    if (r) await write(r);
  }
  for (const rec of [...byId.values()]) {
    const v = viewById.get(rec.covenantId);
    const res = resolved.get(rec.covenantId);
    let next = rec;
    if (rec.cancelling) {
      const c = rec.cancelling;
      const where = v && v.state_known && v.current ? `${v.current.txid}:${v.current.index}` : res?.status === 'live' && res.order ? `${res.order.transactionId}:${res.order.index}` : null;
      const gone = (!!v && TERMINAL_VIEW.has(v.status)) || (!v && res?.status === 'spent');
      const stale = nowUnix !== null && nowUnix >= BigInt(c.atUnix) + CANCEL_PENDING_SECONDS;
      if (gone || (stale && where === null && !(v && isLiveView(v)))) {
        await drop(rec.covenantId);
        continue;
      }
      // the order lives on: a fill won the race (another outpoint), or the cancel never got in (still at the outpoint it spent, long after)
      if ((where !== null && where !== c.spends) || (stale && where === c.spends)) next = { ...next, cancelling: null };
    }
    if (v && isLiveView(v)) next = refreshFromView(s.kob, next, v);
    if (res && (!v || !v.state_known)) next = refreshFromResolved(s.kob, next, res);
    if (next !== rec) await write(next);
  }
  return [...byId.values()];
}

export async function loadOrders(s: Services, pubkey: Hex, store: RecordStore, o: LoadOrdersOptions = {}): Promise<OrdersData> {
  const signal = o.signal;
  const sig = signal ? { signal } : {};
  let indexerError: Error | null = null;
  let nodeError: Error | null = null;
  let straysError: Error | null = null;

  const clockP: Promise<Clock | null> = readClock(s).catch((e) => ((nodeError = toError(e)), null));
  const viewsP: Promise<OrderView[]> = s.indexer
    ? loadMakerOrders(s.indexer, pubkey, sig).catch((e) => ((indexerError = toError(e)), [] as OrderView[]))
    : Promise.resolve([]);
  const straysP: Promise<StrayView[]> = s.indexer
    ? s.indexer.strays({ maker: pubkey, limit: 500 }, sig).catch((e) => ((straysError = toError(e)), [] as StrayView[]))
    : Promise.resolve([]);
  const recordsP = store.list();
  const background = !!o.onUpdate;
  // the node's clock is not waited for long when the list may follow later
  const [clock0, views, strays, stored] = await Promise.all([background ? within(clockP, CLOCK_WAIT_MS) : clockP, viewsP, straysP, recordsP]);

  // records the indexer does not list (or lists live without a proven state): ask the node what became of them (fresh placements, indexer down)
  const unknown = recordsToResolve(views, stored);
  const at = Date.now();
  const verdicts = verdictsOf(s.node);
  const known = new Map<Hex, ResolvedRecord>();
  const pending: PlacementRecord[] = [];
  for (const r of unknown) {
    const v = cachedVerdict(verdicts, r, at);
    if (v) known.set(r.covenantId, v);
    else pending.push(r);
  }

  const assemble = async (resolved: ReadonlyMap<Hex, ResolvedRecord>, failures: number, clock: Clock | null, base: readonly PlacementRecord[]): Promise<OrdersData> => {
    const err = failures > 0 && pending.length === failures && !nodeError ? new Error('the node could not resolve the placement records') : nodeError;
    const records = await syncRecords(s, pubkey, store, base, views, resolved, clock ? clock.unixSeconds : null);
    const snapshots: OrderSnapshot[] = [];
    for (const r of resolved.values()) {
      if (r.status !== 'live') continue;
      try {
        snapshots.push(snapshotFromRecord(r));
      } catch {
        /* not spendable after all */
      }
    }
    return {
      entries: buildEntries(views, records, resolved), views, positions: groupPositions(views), strays, straysError, clock, indexerError, nodeError: err, records, snapshots, resolveFailures: failures,
    };
  };
  const lookUp = async (): Promise<{ resolved: Map<Hex, ResolvedRecord>; failures: number }> => {
    const resolved = new Map(known);
    let failures = 0;
    const found = pending.length ? await resolveRecords(s, pending, o.spkToAddress) : new Map<Hex, ResolvedRecord | null>();
    const now = Date.now();
    for (const r of pending) {
      const res = found.get(r.covenantId) ?? null;
      if (!res) {
        failures++;
        continue;
      }
      resolved.set(r.covenantId, res);
      verdicts.set(verdictKey(r), { res, at: now });
    }
    return { resolved, failures };
  };

  if (!background) {
    const { resolved, failures } = await lookUp();
    return assemble(resolved, failures, clock0, stored);
  }
  // the list now, from the indexer, the records and the verdicts on hand; the node's answers follow
  const first = await assemble(known, 0, clock0, stored);
  if (pending.length || !clock0) {
    void (async () => {
      const [{ resolved, failures }, clock] = await Promise.all([lookUp(), clock0 ? Promise.resolve(clock0) : clockP]);
      if (signal?.aborted) return;
      const next = await assemble(resolved, failures, clock, first.records);
      if (!signal?.aborted) o.onUpdate!(next);
    })().catch(() => undefined);
  }
  return first;
}

/** Every event of one order (`/v1/orders/{id}/events`, following the cursor; at most 10 pages of 100). An unknown order has none. */
export async function loadOrderEvents(indexer: Pick<IndexerApi, 'orderEvents'>, id: Hex, signal?: AbortSignal): Promise<EventView[]> {
  const out: EventView[] = [];
  let after: string | undefined;
  for (let page = 0; page < 10; page++) {
    const r = await indexer.orderEvents(id, { limit: 100, ...(after !== undefined ? { after } : {}) }, signal ? { signal } : {});
    if (!r) break;
    out.push(...r.items);
    if (!r.next_cursor || r.items.length === 0) break;
    after = r.next_cursor;
  }
  return out;
}
