// Wallet adapter contract. Adapters only sign: they never build, never broadcast, and never see anything but the tx the app already
// showed on the pre-sign confirmation screen. Proven on TN10 in tools/wallet-gate (RESULTS.md).
import type { BuiltTx, Hex, InputSignature } from '../kob/types';

export type WalletId = 'kasware' | 'kaspire' | 'kastle';

export interface WalletInfo {
  id: WalletId;
  label: string;
  /** account address, as the wallet reports it */
  address: string;
  /** x-only public key, 64 hex chars (adapters normalise 33-byte compressed keys) */
  pubkey: Hex;
  /** normalised: 'mainnet' | 'testnet-10' | other as reported */
  network: string;
  version: string;
}

/**
 * A change the wallet reports AFTER connect. The payload is only a hint: the app answers every event by re-reading the wallet
 * (`WalletAdapter.refresh`), so a provider that emits odd payloads cannot make the app believe a wrong account or network.
 */
export type WalletEvent =
  | { kind: 'accounts'; accounts: string[] }
  | { kind: 'network'; network: string }
  | { kind: 'disconnect' };

export interface WalletSubscribeOptions {
  /** poll interval, ms, of the fallback used when the provider has no event emitter (default 4000) */
  pollMs?: number;
}

export interface WalletAdapter {
  readonly id: WalletId;
  readonly label: string;
  /**
   * Whether the wallet signs covenant (P2SH) inputs, which every cancel needs (an order's placement needs only P2PK signatures: C5-10). 'proven':
   * verified for this wallet; 'unknown' (or absent): the app asks for a one-time test signature (never broadcast) before the first order.
   */
  readonly covenantSigning?: 'proven' | 'unknown';
  /** provider injected in this page? (extensions inject late: callers poll / re-check) */
  detect(): boolean;
  /** connects and reads account, key and network; tries to switch to `wantNetwork`; the result's `network` tells whether that worked */
  connect(wantNetwork: string): Promise<WalletInfo>;
  /**
   * Asks the wallet to sign every input listed in `built.sign` (SIGHASH_ALL) and returns one signature per request, in the same order.
   * The wallet is given the unsigned tx (kaspa safe JSON) and must not be able to change what the app displayed; the returned signatures
   * are later verified against the digests by kob-wasm `finalize`. Rejects with `WalletError` on refusal / timeout / missing signature.
   */
  signTx(built: BuiltTx, opts?: SignOptions): Promise<InputSignature[]>;
  /**
   * Re-reads account, key and network of the ALREADY connected wallet without asking the user (no connect popup, no network switch).
   * Rejects with `WalletError` when the wallet no longer exposes an account (locked, disconnected the site).
   */
  refresh?(): Promise<WalletInfo>;
  /**
   * Listens for account / network changes of the connected wallet (provider events; a cheap poll while the tab is visible when the
   * provider has no emitter). Call after `connect`. Returns an idempotent unsubscribe that removes every listener / timer.
   */
  subscribe?(onEvent: (e: WalletEvent) => void, opts?: WalletSubscribeOptions): () => void;
}

export interface SignOptions {
  /** network name used by wallets that need it (Kastle) */
  network?: string;
  timeoutMs?: number;
}

export class WalletError extends Error {
  readonly code: 'rejected' | 'timeout' | 'no-signature' | 'unsupported' | 'network' | 'other';
  constructor(code: WalletError['code'], message: string) {
    super(message);
    this.name = 'WalletError';
    this.code = code;
  }
}
