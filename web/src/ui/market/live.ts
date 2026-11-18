// Live updates of the token page. The WebSocket feed (`book:<token>`, `fills:<token>`, `resync`,
// `reorg`) has a 5 s polling fallback while the socket is not open. The feed only says "something
// changed". Every notice triggers a debounced refetch of the REST resources, so the screen always
// shows what the indexer's REST API says, never a half-applied delta. A hidden page (background
// tab) neither polls nor refetches. It remembers that it missed a notice and refreshes once when
// shown again.
import { useEffect, useRef, useState } from 'preact/hooks';
import type { Services } from '../../app/services';
import type { FeedStatus } from '../../data/indexer';
import { onVisibilityChange, pageVisible, useInterval } from '../kit/hooks';
import { useStandalone } from '../shell/standalone';

export type LiveMode = 'live' | 'polling' | 'off';

export const POLL_MS = 5000;
export const DEBOUNCE_MS = 1000;

/**
 * `live` while the socket is open, `polling` otherwise (no feed, connecting, reconnecting or
 * closed), and `off` without an indexer.
 */
export function liveMode(hasIndexer: boolean, status: FeedStatus | null): LiveMode {
  if (!hasIndexer) return 'off';
  return status === 'open' ? 'live' : 'polling';
}

/**
 * What a feed notice does while the page is shown or hidden. It is pure. `full` marks a `reorg` or
 * `resync`, where history may have changed and not only grown. A view that loads incrementally,
 * such as the candles, then reloads in full.
 */
export class LiveGate {
  private missed: 'notice' | 'full' | null = null;
  constructor(private visible: boolean) {}

  /**
   * A notice. True means refresh now (debounced). When hidden it is remembered for when the page is
   * shown, and false.
   */
  notice(full: boolean): boolean {
    if (this.visible) return true;
    if (full || this.missed === null) this.missed = full ? 'full' : 'notice';
    return false;
  }

  /**
   * The page was shown or hidden. It returns the refresh due now, when a notice was missed while
   * hidden, else null.
   */
  shown(visible: boolean): { full: boolean } | null {
    this.visible = visible;
    if (!visible || this.missed === null) return null;
    const full = this.missed === 'full';
    this.missed = null;
    return { full };
  }
}

/**
 * In standalone mode (data/standalone.ts) the indexer's feed is not used. The views poll what the
 * node stream saw, in memory, every `pollMs`.
 *
 * Calls `refresh(full)` when the feed reports a change on `channels` (debounced) and, while the
 * socket is not open, every `pollMs` (default 5 s). It subscribes on mount and unsubscribes on
 * unmount. A hidden page skips both and refreshes once when shown, if it missed anything
 * (`LiveGate`). `o.background` keeps it live while hidden, for the notifications. It returns the
 * current mode.
 */
export function useLiveRefresh(
  s: Pick<Services, 'feed' | 'indexer' | 'standalone'>,
  channels: readonly string[],
  /** `order` is the covenant id a fill notice names, for fills only. */
  relevant: (e: { token?: string | null; order?: string }) => boolean,
  refresh: (full: boolean) => void,
  pollMs: number = POLL_MS,
  o: { background?: boolean } = {},
): LiveMode {
  const sa = useStandalone(s.standalone);
  const feedOf = sa?.active ? null : s.feed;
  const [status, setStatus] = useState<FeedStatus | null>(feedOf ? feedOf.status : null);
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const relevantRef = useRef(relevant);
  relevantRef.current = relevant;
  const key = channels.join('|');
  const background = !!o.background;

  useEffect(() => {
    const feed = feedOf;
    if (!feed) return;
    setStatus(feed.status);
    const gate = new LiveGate(background || pageVisible());
    let timer: ReturnType<typeof setTimeout> | null = null;
    let full = false;
    const kick = (isFull: boolean) => {
      if (!gate.notice(isFull)) return;
      full ||= isFull;
      if (timer) return;
      timer = setTimeout(() => {
        timer = null;
        const f = full;
        full = false;
        refreshRef.current(f);
      }, DEBOUNCE_MS);
    };
    try {
      feed.subscribe([...channels]);
      feed.connect();
    } catch {
      /* Invalid channel or no WebSocket support. Polling covers it. */
    }
    const offs = [
      feed.on('status', (e) => setStatus(e.status)),
      feed.on('book', (n) => relevantRef.current({ token: n.token }) && kick(false)),
      feed.on('fill', (e) => relevantRef.current({ token: e.fill.token, order: e.fill.order }) && kick(false)),
      feed.on('resync', () => kick(true)),
      feed.on('reorg', () => kick(true)),
      background
        ? () => {}
        : onVisibilityChange((visible) => {
            const due = gate.shown(visible);
            if (due) refreshRef.current(due.full);
          }),
    ];
    return () => {
      offs.forEach((off) => off());
      if (timer) clearTimeout(timer);
      try {
        feed.unsubscribe([...channels]);
      } catch {
        /* Socket already gone. */
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [feedOf, key, background]);

  const mode = liveMode(!!s.indexer, feedOf ? status : null);
  // The poll pauses on a hidden page and catches up when it is shown (useInterval).
  useInterval(() => refreshRef.current(false), pollMs, mode === 'polling', { background });
  return mode;
}
