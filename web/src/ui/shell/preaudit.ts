// The open review notice (its names here keep the older word, so a dismissal stays remembered): the specification is not final and no review can show the code is free of bugs, so the app says so on every page (footer, header badge) and, until dismissed,
// in the current status of the bell. A dismissal is remembered in localStorage; blocked storage never breaks the page (the notice then shows again on the
// next visit, which is the safe direction).
import { useState } from 'preact/hooks';

export const PREAUDIT_KEY = 'kob.preAudit.dismissed.v1';
/** The security policy (SECURITY.md), linked from the footer notice. */
export const SECURITY_POLICY_URL = 'https://github.com/RossKU/KOB/blob/master/SECURITY.md';

type Store = Pick<Storage, 'getItem' | 'setItem'>;

function defaultStore(): Store | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

/** True when the visitor dismissed the notice before. Any storage failure counts as "not dismissed". */
export function isPreAuditDismissed(store: Store | null = defaultStore()): boolean {
  try {
    return store?.getItem(PREAUDIT_KEY) === '1';
  } catch {
    return false;
  }
}

/** Remembers the dismissal; returns false when the storage refused (the notice is still hidden for this page view). */
export function dismissPreAudit(store: Store | null = defaultStore()): boolean {
  try {
    if (!store) return false;
    store.setItem(PREAUDIT_KEY, '1');
    return true;
  } catch {
    return false;
  }
}

/** `[visible, dismiss]` of the notice in the bell's current status. */
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
