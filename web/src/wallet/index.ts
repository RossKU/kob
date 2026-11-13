// Wallet layer entry point: the adapters bound to `window`, plus the pieces the UI and the signing
// pipeline use.
import type { ExtensionWalletId, WalletAdapter } from './types';
import { createKaswareAdapter } from './kasware';
import { createKaspireAdapter } from './kaspire';
import { createKastleAdapter } from './kastle';

export * from './types';
export { createKaswareAdapter, type KaswareProvider } from './kasware';
export { createKaspireAdapter, dispatchTagFrom, kaspireSignatureScript, type KaspireProvider } from './kaspire';
export { createKastleAdapter, type KastleProvider } from './kastle';
export { createLocalKeyAdapter, SignerHost, LOCAL_KEY_LABEL } from './local-key';
export { SIGN_TIMEOUT_MS, extractSignature, errText, isUserRejection, normalizePubkey, parsePushes } from './sigs';
export { createAdapters, discoverWallets, watchWallets, type WalletHost } from './discover';
export { signAndSubmit, waitForAcceptance, expectedOutputs, SignFlowError } from './sign';

/**
 * The default adapters. They read `window.kasware`, `window.kaspire`, `window.kastle` when called.
 */
export const ADAPTERS: Record<ExtensionWalletId, WalletAdapter> = {
  kasware: createKaswareAdapter(),
  kaspire: createKaspireAdapter(),
  kastle: createKastleAdapter(),
};

/** The adapters offered to the user. Kastle is included only behind `features.kastle`. */
export function enabledAdapters(features: { kastle: boolean }): WalletAdapter[] {
  return [ADAPTERS.kasware, ADAPTERS.kaspire, ...(features.kastle ? [ADAPTERS.kastle] : [])];
}
