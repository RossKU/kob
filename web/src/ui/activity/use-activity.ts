// The activity log's live data (Preact): an ActivityStore per market filter, fed by the WebSocket feed and polls.
//   * fills: a `fill` notice (`fills` / `fills:<token>`; a pair: its two tokens' `book:` notices) triggers a debounced read of the newest page
//     (useLiveRefresh); without the socket the same read runs every POLL_MS;
//   * placements: the indexer sends no notice for them (phase 2: an `events` channel), so they are polled every POLL_MS while they are shown;
//   * standalone mode: what the page saw on the node, read again whenever it changes ("since HH:MM", nothing older);
//   * the chain's DAA score (the confirmation pies) from the feed's `health` frames, else from the records' own confirmations.
// A reorg or a reconnect reads everything again from the newest page.
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { displayToken } from '../../app/token-display';
import type { PairFillsView } from '../../data/indexer-types';
import type { Hex } from '../../kob/types';
import { scaleOfDecimals, useInterval } from '../kit';
import { useLiveRefresh, type LiveMode } from '../market/live';
import { useStandalone } from '../shell/standalone';
import { marketKeyOf, type ActivityKind, type ActivityRow, type MarketFilter, type OrderFacts } from './activity-model';
import { ActivityStore } from './activity-store';

/** Polls of the placements (and of the fills while the socket is down), ms. */
export const POLL_MS = 10_000;

export interface ActivityData {
  rows: ActivityRow[];
  order: (id: Hex) => OrderFacts | null | undefined;
  /** the first read is running and nothing is in yet */
  loading: boolean;
  /** the first read is running, whatever is in */
  firstRead: boolean;
  /** the first read failed (later failures keep the rows and try again on the next notice or poll) */
  error: Error | null;
  retry(): void;
  mode: LiveMode | 'standalone';
  /** standalone mode: since when the page has been watching the node */
  sinceMs: number | null;
  hasOlder: boolean;
  loadingOlder: boolean;
  loadOlder(): void;
  /** the chain's DAA score when last reported */
  chain: { daa: number; atMs: number } | null;
  /** looks up the orders of these rows (the Wallet column) */
  want(ids: readonly Hex[]): void;
}

export function useActivity(o: {
  market: MarketFilter;
  kinds: ReadonlySet<ActivityKind>;
  pairFills?: { base: Hex; quote: Hex; view: PairFillsView | null | undefined } | null;
  /**
   * the list is on screen (default true): off screen nothing is read (a notice or a poll is remembered and read once it is shown), so a log below the
   * fold of a token page never takes a connection the order ticket needs
   */
  onScreen?: boolean;
}): ActivityData {
  const services = useServices();
  const { registry, config } = services;
  const sa = useStandalone(services.standalone);
  const standalone = sa?.active ? services.standalone ?? null : null;
  const [, setTick] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [resetKey, setResetKey] = useState(0);
  const key = marketKeyOf(o.market);
  const placed = o.kinds.has('placed');
  const kindsRef = useRef(o.kinds);
  kindsRef.current = o.kinds;

  // the market's price scale of a token: its standard scale when its decimals are known (registry, declaration, program default, issuance); a token
  // shown with assumed decimals waits for its orders' own scale (the Wallet column's lookups)
  const tokenScale = useMemo(
    () => (token: Hex): bigint | null => {
      const info = displayToken(registry, config.network, token);
      return info && info.decimalsSource !== 'assumed' ? scaleOfDecimals(info.decimals) : null;
    },
    [registry, config.network],
  );

  const store = useMemo(
    () => new ActivityStore(o.market, { indexer: services.indexer, standalone, pairFills: o.pairFills ?? null }, tokenScale, () => setTick((n) => n + 1)),
    // a new market filter, source or reset starts over
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key, services.indexer, standalone, tokenScale, resetKey],
  );
  useEffect(() => () => store.dispose(), [store]);
  useEffect(() => store.setPairFills(o.pairFills ?? null), [store, o.pairFills?.view]);

  // one read at a time per store; a notice during a read is folded into one more read right after it (of the store current then: a read of a
  // store that was replaced meanwhile, e.g. standalone mode starting while the indexer hangs, never blocks the new one)
  const storeRef = useRef(store);
  storeRef.current = store;
  const flight = useRef<{ store: ActivityStore; busy: boolean; again: boolean } | null>(null);
  const onScreen = o.onScreen ?? true;
  const onScreenRef = useRef(onScreen);
  onScreenRef.current = onScreen;
  const missed = useRef(false);
  /** the next read replaces the newest page (a reorg or a reconnect), see ActivityStore.readNewest */
  const replaceNext = useRef(false);
  const read = (asReplace = false) => {
    if (asReplace) replaceNext.current = true;
    if (!onScreenRef.current) {
      missed.current = true;
      return;
    }
    missed.current = false;
    const s = storeRef.current;
    if (flight.current?.store !== s) flight.current = { store: s, busy: false, again: false };
    const f = flight.current;
    if (f.busy) {
      f.again = true;
      return;
    }
    f.busy = true;
    const replace = replaceNext.current;
    replaceNext.current = false;
    s.readNewest({ fills: true, placed: kindsRef.current.has('placed'), replace })
      .then(() => {
        if (storeRef.current === s) setError(null);
      })
      .catch((e: unknown) => {
        if (storeRef.current === s && s.fills.size === 0 && s.orders.size === 0 && !(e instanceof Error && e.name === 'AbortError')) setError(e instanceof Error ? e : new Error(String(e)));
      })
      .finally(() => {
        f.busy = false;
        if (storeRef.current !== s) return;
        setLoading(false);
        if (f.again) {
          f.again = false;
          read();
        }
      });
  };

  // shown again after a notice or a poll was missed (or before the first read): read now
  useEffect(() => {
    if (onScreen && missed.current) read();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onScreen]);

  // the first read of this store
  useEffect(() => {
    setLoading(true);
    setError(null);
    read();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store]);
  // placements switched on: their first page
  useEffect(() => {
    if (placed && store.ordersCursor === undefined && !loading) read();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [placed]);

  const m = o.market;
  const channels = m.kind === 'all' ? ['fills'] : m.kind === 'token' ? [`fills:${m.token}`] : [`book:${m.base}`, `book:${m.quote}`];
  const mode = useLiveRefresh(
    services,
    channels,
    (e) => m.kind === 'all' || !e.token || (m.kind === 'token' ? e.token === m.token : e.token === m.base || e.token === m.quote),
    // a reorg or a reconnect: the newest page again, what it no longer lists is dropped; the list is never emptied and rebuilt (no flicker)
    (full) => read(full),
    POLL_MS,
  );
  // the placements have no notice: polled while shown and the socket carries the fills
  useInterval(() => read(), POLL_MS, placed && mode === 'live' && !standalone);
  // standalone: every change of what was seen
  useEffect(() => {
    if (standalone) read();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sa?.version]);

  // the chain's DAA score from the feed's health frames
  useEffect(() => {
    const feed = standalone ? null : services.feed;
    if (!feed) return;
    return feed.on('health', (h) => {
      const d = h.data as { node_daa?: number | null; cursor_daa?: number };
      const daa = typeof d.node_daa === 'number' ? d.node_daa : typeof d.cursor_daa === 'number' ? d.cursor_daa : null;
      if (daa !== null) store.noteChain(daa);
    });
  }, [services.feed, standalone, store]);

  const rows = store.rows(o.kinds);
  return {
    rows,
    order: (id) => (store.facts.has(id) ? store.facts.get(id)! : undefined),
    loading: loading && rows.length === 0,
    firstRead: loading,
    error,
    retry: () => setResetKey((k) => k + 1),
    mode: standalone ? 'standalone' : mode,
    sinceMs: standalone ? sa?.sinceMs ?? null : null,
    hasOlder: store.hasOlder(o.kinds),
    loadingOlder,
    loadOlder: () => {
      if (loadingOlder) return;
      setLoadingOlder(true);
      store
        .readOlder(kindsRef.current)
        .catch(() => undefined)
        .finally(() => setLoadingOlder(false));
    },
    chain: store.chain,
    want: (ids) => store.want(ids),
  };
}
