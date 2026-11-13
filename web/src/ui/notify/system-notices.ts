// System notifications: what the app itself did that the user should know of, with or without a wallet (the wallet's notifications are per wallet,
// kob/notifications.ts). Today: the market switched to the user's node (standalone mode, data/standalone.ts), and back to the indexer. One notification
// per switch, in the bell with the others; nothing else on the page announces it (the header's dot is yellow while it lasts). Kept for the session only.
import type { StandaloneState } from '../../data/standalone';

export type SystemNoticeKind = 'node' | 'indexer';

export interface SystemNotice {
  id: string;
  kind: SystemNoticeKind;
  /** `node`: why (standalone mode switched on in Settings, or the indexer could not be used) */
  reason: 'manual' | 'auto' | null;
  /** `node`, automatic: the indexer was behind the chain (not unreachable) */
  behind?: boolean;
  at: number;
  read: boolean;
}

/** Most system notifications kept (the oldest go first). */
export const MAX_SYSTEM_NOTICES = 20;

let seq = 0;

/** The notification a change of standalone mode makes (`prev` -> `next`), or null: nothing switched. */
export function noticeOfSwitch(prev: Pick<StandaloneState, 'active' | 'reason'> & { behind?: boolean }, next: Pick<StandaloneState, 'active' | 'reason'> & { behind?: boolean }, at: number): SystemNotice | null {
  if (prev.active === next.active) return null;
  return { id: `system-${at}-${++seq}`, kind: next.active ? 'node' : 'indexer', reason: next.active ? next.reason : null, ...(next.active && next.behind ? { behind: true } : {}), at, read: false };
}

/** `list` with `n` first, at most MAX_SYSTEM_NOTICES. */
export function addNotice(list: readonly SystemNotice[], n: SystemNotice): SystemNotice[] {
  return [n, ...list].slice(0, MAX_SYSTEM_NOTICES);
}
