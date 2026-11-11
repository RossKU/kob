// The extension wallets the connect chooser knows (pure, tested in wallet-model.test.ts): the ones it offers to install when the browser has not got them
// (review L-07). Detection itself is wallet/discover.ts; this only names the wallets and where a person gets one. The links are the wallets' own home pages,
// never a store listing the app could not vouch for.
import type { ExtensionWalletId } from '../../wallet/types';

export interface KnownWallet {
  id: ExtensionWalletId;
  label: string;
  /** where to get the extension */
  installUrl: string;
}

export const KNOWN_WALLETS: readonly KnownWallet[] = [
  { id: 'kasware', label: 'KasWare', installUrl: 'https://www.kasware.xyz/' },
  { id: 'kaspire', label: 'Kaspire', installUrl: 'https://kaspa.org/wallets/' },
  { id: 'kastle', label: 'Kastle', installUrl: 'https://kaspa.org/wallets/' },
];

/** The wallets to offer for install: every known one that was not detected (Kastle only where it is switched on, `features.kastle`). */
export function installable(detectedIds: readonly string[], kastle: boolean): KnownWallet[] {
  return KNOWN_WALLETS.filter((w) => !detectedIds.includes(w.id) && (w.id !== 'kastle' || kastle));
}
