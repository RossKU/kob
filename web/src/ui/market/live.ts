// Live updates of the token page: WebSocket feed (`book:<token>`, `fills:<token>`, `resync`, `reorg`) with a 5 s polling fallback while the
// socket is not open. The feed only says "something changed": every notice triggers a (debounced) REFETCH of the REST resources, so the
// screen always shows what the indexer's REST API says, never a half-applied delta.
import { useEffect, useRef, useState } from 'preact/hooks';
import type { Services } from '../../app/services';
import type { FeedStatus } from '../../data/indexer';

export type LiveMode = 'live' | 'polling' | 'off';

export const POLL_MS = 5000;
export const DEBOUNCE_MS = 1000;

/** `live` while the socket is open, `polling` otherwise (no feed, connecting, reconnecting, closed), `off` without an indexer. */
export function liveMode(hasIndexer: boolean, status: FeedStatus | null): LiveMode {
  if (!hasIndexer) return 'off';
  return status === 'open' ? 'live' : 'polling';
}

/**
 * Calls `refresh` when the feed reports a change on `channels` (debounced) and, while the socket is not open, every `pollMs` (default 5 s).
 * Subscribes on mount, unsubscribes on unmount. Returns the current mode.
 */
export function useLiveRefresh(
  s: Pick<Services, 'feed' | 'indexer'>,
  channels: readonly string[],
  relevant: (e: { token?: string | null }) => boolean,
  refresh: () => void,
  pollMs: number = POLL_MS,
): LiveMode {
  const [status, setStatus] = useState<FeedStatus | null>(s.feed ? s.feed.status : null);
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const relevantRef = useRef(relevant);
  relevantRef.current = relevant;
  const key = channels.join('|');

  useEffect(() => {
    const feed = s.feed;
    if (!feed) return;
    setStatus(feed.status);
    let timer: ReturnType<typeof setTimeout> | null = null;
    const kick = () => {
      if (timer) return;
      timer = setTimeout(() => {
        timer = null;
        refreshRef.current();
      }, DEBOUNCE_MS);
    };
    try {
      feed.subscribe([...channels]);
      feed.connect();
    } catch {
      /* invalid channel or no WebSocket support: polling covers it */
    }
    const offs = [
      feed.on('status', (e) => setStatus(e.status)),
      feed.on('book', (n) => relevantRef.current({ token: n.token }) && kick()),
      feed.on('fill', (e) => relevantRef.current({ token: e.fill.token }) && kick()),
      feed.on('resync', kick),
      feed.on('reorg', kick),
    ];
    return () => {
      offs.forEach((off) => off());
      if (timer) clearTimeout(timer);
      try {
        feed.unsubscribe([...channels]);
      } catch {
        /* socket already gone */
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s.feed, key]);

  const mode = liveMode(!!s.indexer, s.feed ? status : null);
  useEffect(() => {
    if (mode !== 'polling') return;
    const id = setInterval(() => refreshRef.current(), pollMs);
    return () => clearInterval(id);
  }, [mode, pollMs]);
  return mode;
}
