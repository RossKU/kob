// The pre-audit notice: the contracts have had no external security review, so the app says so on every page (footer, header badge) and once, on the
// first visit, in a dismissible banner. A dismissal is remembered in localStorage; blocked storage never breaks the page (the banner then shows again
// on the next visit, which is the safe direction).
import { useState } from 'preact/hooks';

export const PREAUDIT_KEY = 'kob.preAudit.dismissed.v1';
/** The security policy (SECURITY.md), linked from the footer notice. */
export const SECURITY_POLICY_URL = 'https://github.com/RossKU/KOB/blob/main/SECURITY.md';

type Store = Pick<Storage, 'getItem' | 'setItem'>;

function defaultStore(): Store | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

/** True when the visitor dismissed the first-visit banner before. Any storage failure counts as "not dismissed". */
export function isPreAuditDismissed(store: Store | null = defaultStore()): boolean {
  try {
    return store?.getItem(PREAUDIT_KEY) === '1';
  } catch {
    return false;
  }
}

/** Remembers the dismissal; returns false when the storage refused (the banner is still hidden for this page view). */
export function dismissPreAudit(store: Store | null = defaultStore()): boolean {
  try {
    if (!store) return false;
    store.setItem(PREAUDIT_KEY, '1');
    return true;
  } catch {
    return false;
  }
}

/** `[visible, dismiss]` of the first-visit banner. */
export function usePreAuditBanner(): [boolean, () => void] {
  const [visible, setVisible] = useState(() => !isPreAuditDismissed());
  return [
    visible,
    () => {
      dismissPreAudit();
      setVisible(false);
    },
  ];
}
