// Live system status for the status bar and the banners: node reachability (+ virtual DAA), indexer health (polled and pushed by the feed),
// the feed's connection state. Every failure is data (`error`), never an exception: a down node or indexer degrades the UI, it does not blank it.
import { useCallback, useEffect, useRef, useState } from 'preact/hooks';
import type { Services } from '../../app/services';
import type { FeedStatus } from '../../data/indexer';
import type { HealthView } from '../../data/indexer-types';
import { toError, useInterval } from '../kit/hooks';
import { assessIndexerHealth, type HealthAssessment } from './health';
import { isDeepReorg } from './reorg-notice';

export const HEALTH_POLL_MS = 10_000;

export interface IndexerStatus {
  health: HealthView | null;
  error: Error | null;
  assessment: HealthAssessment;
  refresh(): void;
}

/** Polls `indexer.health()` and refreshes it on every `health` / `reorg` push of the feed. */
export function useIndexerStatus(s: Pick<Services, 'indexer' | 'feed' | 'config'>): IndexerStatus {
  const [health, setHealth] = useState<HealthView | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const inflight = useRef<AbortController | null>(null);

  const refresh = useCallback(() => {
    if (!s.indexer) return;
    inflight.current?.abort();
    const ctl = new AbortController();
    inflight.current = ctl;
    s.indexer.health({ signal: ctl.signal }).then(
      (h) => {
        if (ctl.signal.aborted) return;
        setHealth(h);
        setError(null);
      },
      (e) => {
        if (!ctl.signal.aborted) setError(toError(e));
      },
    );
  }, [s.indexer]);

  useEffect(() => {
    refresh();
    return () => inflight.current?.abort();
  }, [refresh]);
  useInterval(refresh, HEALTH_POLL_MS, !!s.indexer);

  useEffect(() => {
    if (!s.feed) return;
    let last = 0;
    const throttled = () => {
      // a busy chain pushes one `cursor` frame per block: refetch at most once per second
      const now = Date.now();
      if (now - last > 1000) {
        last = now;
        refresh();
      }
    };
    const offs = [s.feed.on('health', throttled), s.feed.on('reorg', refresh), s.feed.on('resync', refresh)];
    return () => offs.forEach((off) => off());
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

/** Polls the node clock (virtual DAA). */
export function useNodeStatus(s: Pick<Services, 'node'>): NodeStatus {
  const [st, setSt] = useState<NodeStatus>({ reachable: undefined, daa: null, error: null });
  const poll = useCallback(() => {
    s.node.getClock().then(
      (c) => setSt({ reachable: true, daa: c.daa, error: null }),
      (e) => setSt((prev) => ({ reachable: false, daa: prev.daa, error: toError(e) })),
    );
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
