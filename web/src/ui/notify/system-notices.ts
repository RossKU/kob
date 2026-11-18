// System notifications. They are what the app itself did that the user should know of, with or
// without a wallet. The wallet's notifications are per wallet (kob/notifications.ts).
// There are two kinds:
// - The market switched to the user's node (standalone mode, data/standalone.ts) and back to the
//   indexer. Each switch is one notification, in the bell with the others. Nothing else on the
//   page announces it, and the header's dot is yellow while it lasts.
// - An error toast raised while a flow's card is on screen (kit/toast-store.ts `hold`). The toast
//   shows too. The notification colours the bell until it is read.
// They are kept for the session only.
import type { StandaloneState } from '../../data/standalone';

export type SystemNoticeKind = 'node' | 'indexer' | 'error';

export interface SystemNotice {
  id: string;
  kind: SystemNoticeKind;
  /** `node`: why. Standalone mode was switched on in Settings, or the indexer could not be used. */
  reason: 'manual' | 'auto' | null;
  /** `node`, automatic: the indexer was behind the chain, not unreachable. */
  behind?: boolean;
  /**
   * `error`: the text of an error toast held back while a flow's card was on screen
   * (kit/toast-store.ts `hold`).
   */
  text?: string;
  at: number;
  read: boolean;
}

/** The most system notifications kept. The oldest go first. */
export const MAX_SYSTEM_NOTICES = 20;

let seq = 0;

/**
 * The notification a change of standalone mode makes (`prev` to `next`), or null when nothing
 * switched.
 */
export function noticeOfSwitch(prev: Pick<StandaloneState, 'active' | 'reason'> & { behind?: boolean }, next: Pick<StandaloneState, 'active' | 'reason'> & { behind?: boolean }, at: number): SystemNotice | null {
  if (prev.active === next.active) return null;
  return { id: `system-${at}-${++seq}`, kind: next.active ? 'node' : 'indexer', reason: next.active ? next.reason : null, ...(next.active && next.behind ? { behind: true } : {}), at, read: false };
}

/**
 * The notification of an error toast a flow's card held back. It is in the bell, unread, and
 * colours it until read.
 */
export function noticeOfHeldError(text: string, at: number): SystemNotice {
  return { id: `system-${at}-${++seq}`, kind: 'error', reason: null, text, at, read: false };
}

/** `list` with `n` first, at most `MAX_SYSTEM_NOTICES` long. */
export function addNotice(list: readonly SystemNotice[], n: SystemNotice): SystemNotice[] {
  return [n, ...list].slice(0, MAX_SYSTEM_NOTICES);
}
