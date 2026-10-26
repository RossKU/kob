// Whether the page runs in a phone or tablet browser (extension wallets do not exist there). A user-agent guess used only to word a hint. Pure.
export interface NavigatorLike {
  userAgent?: string;
  platform?: string;
  maxTouchPoints?: number;
}

/** iPadOS reports itself as a Mac with a touch screen. */
export function isPhoneBrowser(nav: NavigatorLike | undefined = typeof navigator === 'undefined' ? undefined : navigator): boolean {
  if (!nav) return false;
  return /Android|iPhone|iPad|iPod/i.test(nav.userAgent ?? '') || (nav.platform === 'MacIntel' && (nav.maxTouchPoints ?? 0) > 1);
}
