// Live system status for the status bar and the banners: node reachability (+ virtual DAA), indexer health (polled and pushed by the feed),
// the feed's connection state. Every failure is data (`error`), never an exception: a down node or indexer degrades the UI, it does not blank it.
//
// Load: `/v1/health` is read every HEALTH_POLL_MS on a shown page (a background tab pauses, and catches up when shown), on a reconnect of the feed,
// and, at most once per HEALTH_PUSH_GAP_MS, when the feed reports a reorg, a resync or a new state in its periodic `health` frame. The per-block
// `cursor` frames never trigger a read (a busy chain pushes one per block: that was one read per second per open tab).
import { useCallback, useEffect, useRef, useState } from 'preact/hooks';
import type { Services } from '../../app/services';
import type { FeedStatus } from '../../data/indexer';
import type { HealthTickNotice, HealthView } from '../../data/indexer-types';
import { Coalescer, pageVisible, toError, useInterval } from '../kit/hooks';
import { assessIndexerHealth, type HealthAssessment } from './health';
import { isDeepReorg } from './reorg-notice';

export const HEALTH_POLL_MS = 15_000;
/** feed-triggered health reads (reorg, resync, a state change) are at least this far apart */
export const HEALTH_PUSH_GAP_MS = 10_000;

export interface IndexerStatus {
  health: HealthView | null;
  error: Error | null;
  assessment: HealthAssessment;
  refresh(): void;
}

/** Runs `fn` at most once per `gapMs` (pure apart from the timer): a call inside the gap is deferred to its end, one run however many calls came in. */
export class Throttle {
  private last = -Infinity;
  private timer: ReturnType<typeof setTimeout> | null = null;
  constructor(
    private readonly fn: () => void,
    private readonly gapMs: number,
    private readonly now: () => number = Date.now,
  ) {}

  call(): void {
    if (this.timer) return;
    const wait = this.last + this.gapMs - this.now();
    if (wait <= 0) return this.fire();
    this.timer = setTimeout(() => {
      this.timer = null;
      this.fire();
    }, wait);
  }

  /** `fn` ran by other means (the poll): the gap starts again from now. */
  ran(): void {
    this.last = this.now();
  }

  cancel(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  private fire(): void {
    this.last = this.now();
    this.fn();
  }
}

/**
 * Whether a frame of the feed's `health` channel calls for a `/v1/health` read: only the periodic `health` frame whose state differs from the one on
 * screen (a per-block `cursor` frame never does: the REST poll keeps the lag current).
 */
export function healthPushDue(frame: { type: string; data: unknown }, held: HealthView | null): boolean {
  if (frame.type !== 'health') return false;
  const d = frame.data as Partial<HealthTickNotice> | null;
  return !!d && typeof d.state === 'string' && (held === null || d.state !== held.state);
}

/** Polls `indexer.health()` (coalesced: never two reads in flight) and re-reads it on the feed's pushes that matter (see the head of this file). */
export function useIndexerStatus(s: Pick<Services, 'indexer' | 'feed' | 'config'>): IndexerStatus {
  const [health, setHealth] = useState<HealthView | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const alive = useRef(true);
  const runs = useRef(new Coalescer());
  const held = useRef<HealthView | null>(null);
  held.current = health;

  const refresh = useCallback(() => {
    const indexer = s.indexer;
    if (!indexer || !runs.current.request()) return;
    const once = (): void => {
      runs.current.started();
      indexer.health().then(
        (h) => {
          if (!alive.current) return;
          setHealth(h);
          setError(null);
          if (runs.current.settled()) once();
        },
        (e) => {
          if (!alive.current) return;
          setError(toError(e));
          if (runs.current.settled()) once();
        },
      );
    };
    once();
  }, [s.indexer]);

  useEffect(() => {
    alive.current = true;
    refresh();
    return () => {
      alive.current = false;
    };
  }, [refresh]);
  useInterval(refresh, HEALTH_POLL_MS, !!s.indexer);

  useEffect(() => {
    if (!s.feed) return;
    const pushed = new Throttle(refresh, HEALTH_PUSH_GAP_MS);
    pushed.ran(); // the mount just read it
    const onPush = () => {
      if (pageVisible()) pushed.call();
    };
    const offs = [
      s.feed.on('health', (f) => healthPushDue(f, held.current) && onPush()),
      s.feed.on('reorg', onPush),
      // a resync the server sent (the client fell behind); the resync of a reconnect is the `open` below
      s.feed.on('resync', (e) => e.reason !== 'reconnect' && onPush()),
      // the socket was down: whatever happened meanwhile, read it now, once
      s.feed.on('open', (e) => {
        if (e.reconnect && pageVisible()) {
          pushed.ran();
          refresh();
        }
      }),
    ];
    return () => {
      offs.forEach((off) => off());
      pushed.cancel();
    };
  }, [s.feed, refresh]);

  const assessment = assessIndexerHealth({ health, error, configured: !!s.indexer, network: s.config.network });
  return { health, error, assessment, refresh };
}

export interface NodeStatus {
  /** undefined until the first answer */
  reachable: boolean | undefined;
  daa: bigint | null;
  error: Error | null;
}

/**
 * The longest the status waits for the node's answer. The node client has its own timeouts (about 20 s to connect), but a hung connect or
 * resolver lookup (the SDK's public resolver answers a browser with 404 / no CORS headers on testnet-10) must still end as "unreachable" with a
 * reason, never as "checking..." for ever.
 */
export const NODE_WAIT_MS = 25_000;

/** One read of the node clock, ended by `waitMs` at the latest: the status it makes (`error` says what, a timeout included). */
export async function readNodeStatus(node: Pick<Services['node'], 'getClock'>, waitMs: number, prev: NodeStatus | null = null): Promise<NodeStatus> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const limit = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`The node did not answer within ${Math.round(waitMs / 1000)} s`)), waitMs);
  });
  try {
    const c = await Promise.race([node.getClock(), limit]);
    return { reachable: true, daa: c.daa, error: null };
  } catch (e) {
    return { reachable: false, daa: prev?.daa ?? null, error: toError(e) };
  } finally {
    clearTimeout(timer);
  }
}

/** Polls the node clock (virtual DAA). */
export function useNodeStatus(s: Pick<Services, 'node'>): NodeStatus {
  const [st, setSt] = useState<NodeStatus>({ reachable: undefined, daa: null, error: null });
  const busy = useRef(false);
  const poll = useCallback(() => {
    // one read at a time: a node that hangs must not pile up a new request every poll
    if (busy.current) return;
    busy.current = true;
    void readNodeStatus(s.node, NODE_WAIT_MS).then((next) => {
      busy.current = false;
      setSt((prev) => (next.reachable ? next : { ...next, daa: prev.daa }));
    });
  }, [s.node]);
  useEffect(poll, [poll]);
  useInterval(poll, HEALTH_POLL_MS);
  return st;
}

/** Connection state of the WebSocket feed (`idle` when there is no feed). */
export function useFeedStatus(feed: Services['feed']): FeedStatus {
  const [status, setStatus] = useState<FeedStatus>(feed?.status ?? 'idle');
  useEffect(() => {
    if (!feed) return;
    setStatus(feed.status);
    return feed.on('status', (e) => setStatus(e.status));
  }, [feed]);
  return status;
}

export interface DeepReorgs {
  /** deep re-organisations seen since mount */
  count: number;
  /** chain blocks replaced by the latest one */
  blocks: number;
}

/** Deep chain re-organisations (more than `DEEP_REORG_BLOCKS` replaced) seen since mount; shallow ones are routine and not counted. */
export function useDeepReorgs(feed: Services['feed']): DeepReorgs {
  const [s, setS] = useState<DeepReorgs>({ count: 0, blocks: 0 });
  useEffect(() => (feed ? feed.on('reorg', (e) => { if (isDeepReorg(e?.reverted_blocks)) setS((x) => ({ count: x.count + 1, blocks: e.reverted_blocks })); }) : undefined), [feed]);
  return s;
}
