// Wallet layer entry point: the adapters bound to `window`, plus the pieces the UI and the signing pipeline use.
import type { WalletAdapter, WalletId } from './types';
import { createKaswareAdapter } from './kasware';
import { createKaspireAdapter } from './kaspire';
import { createKastleAdapter } from './kastle';

export * from './types';
export { createKaswareAdapter, type KaswareProvider } from './kasware';
export { createKaspireAdapter, dispatchTagFrom, kaspireSignatureScript, type KaspireProvider } from './kaspire';
export { createKastleAdapter, type KastleProvider } from './kastle';
export { SIGN_TIMEOUT_MS, extractSignature, errText, isUserRejection, normalizePubkey, parsePushes } from './sigs';
export { createAdapters, discoverWallets, watchWallets, type WalletHost } from './discover';
export { signAndSubmit, waitForAcceptance, expectedOutputs, SignFlowError } from './sign';

/** Default adapters reading `window.kasware` / `window.kaspire` / `window.kastle` at call time. */
export const ADAPTERS: Record<WalletId, WalletAdapter> = {
  kasware: createKaswareAdapter(),
  kaspire: createKaspireAdapter(),
  kastle: createKastleAdapter(),
};

/** Adapters offered to the user: Kastle only behind `features.kastle`. */
export function enabledAdapters(features: { kastle: boolean }): WalletAdapter[] {
  return [ADAPTERS.kasware, ADAPTERS.kaspire, ...(features.kastle ? [ADAPTERS.kastle] : [])];
}
