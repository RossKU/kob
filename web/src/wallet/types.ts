// The wallet adapter contract.
//
// Adapters sign. They never build, and never see anything but the transaction the app already
// showed on the pre-sign confirmation screen. They broadcast nothing of their own. The one
// exception is `announce`, by which an adapter may hand the wallet's own node a transaction that
// KOB already finalized, validated and sent to its node (wallet/visibility.ts). tools/wallet-gate
// (RESULTS.md) holds the proof on TN10.
import type { BuiltTx, Hex, InputSignature, TxJson } from '../kob/types';
import type { GenesisClaim, LockHint, SignerNote } from '../signer/protocol';

/** Browser extension wallets, discovered on `window`. */
export type ExtensionWalletId = 'kasware' | 'kaspire' | 'kastle';
/**
 * A wallet the app can use. `local-key` is the private key typed into the sandboxed signer frame
 * (wallet/local-key.ts, src/signer).
 */
export type WalletId = ExtensionWalletId | 'local-key';

/** The connected wallet's account, key and network. */
export interface WalletInfo {
  id: WalletId;
  label: string;
  /** The account address, as the wallet reports it. */
  address: string;
  /** The x-only public key, 64 hex chars. Adapters normalise 33-byte compressed keys. */
  pubkey: Hex;
  /** The network, normalised to 'mainnet' or 'testnet-10', or any other as reported. */
  network: string;
  version: string;
}

/**
 * A change the wallet reports after connect. The payload is only a hint. The app answers every
 * event by re-reading the wallet (`WalletAdapter.refresh`), so a provider that emits odd payloads
 * cannot make the app believe a wrong account or network.
 */
export type WalletEvent =
  | { kind: 'accounts'; accounts: string[] }
  | { kind: 'network'; network: string }
  | { kind: 'disconnect' };

/** How a wallet subscription polls when the provider has no event emitter. */
export interface WalletSubscribeOptions {
  /** The poll interval in ms of the fallback. The default is 4000. */
  pollMs?: number;
}

/** What the app asks of a wallet. */
export interface WalletAdapter {
  readonly id: WalletId;
  readonly label: string;
  /**
   * Whether the wallet signs covenant (P2SH) inputs, which every cancel needs. An order's placement
   * needs only P2PK signatures.
   * - `proven`: verified for this wallet.
   * - `unknown`, or absent: the app asks for a one-time test signature, never broadcast, before
   *   the first order.
   */
  readonly covenantSigning?: 'proven' | 'unknown';
  /**
   * What the wallet needs of the inputs it signs.
   * - `node-visible` is the default, for every extension wallet. The wallet looks each input up on
   *   its own node and refuses one it does not find unspent there. KasWare 0.11.5 does so off
   *   mainnet, and KOB assumes every extension wallet does on every network. So the app asks it to
   *   sign only once every input is visible there (wallet/visibility.ts).
   * - `offline`: the wallet signs what it is shown. This is the private-key signer.
   */
  readonly inputPolicy?: 'offline' | 'node-visible';
  /**
   * Tells whether the provider is injected in this page. Extensions inject late, so callers poll
   * and check again.
   */
  detect(): boolean;
  /**
   * Connects and reads the account, key and network, and tries to switch to `wantNetwork`. The
   * result's `network` tells whether that worked.
   */
  connect(wantNetwork: string): Promise<WalletInfo>;
  /**
   * Asks the wallet to sign every input listed in `built.sign` with SIGHASH_ALL. It returns one
   * signature per request, in the same order.
   *
   * The wallet is given the unsigned transaction as Kaspa safe JSON, and must not be able to change
   * what the app displayed. kob-wasm `finalize` later verifies the returned signatures against the
   * digests. It rejects with `WalletError` on a refusal, a timeout or a missing signature.
   */
  signTx(built: BuiltTx, opts?: SignOptions): Promise<InputSignature[]>;
  /**
   * Signs the two transactions of a token creation under one approval. `txs[1]`, the genesis,
   * spends output 0 of `txs[0]`, its KCC-23 preparation.
   *
   * Only a wallet that shows both together on one confirmation offers it, which is the private-key
   * signer. Every other wallet signs each with `signTx`, one approval each, because their extension
   * APIs sign one transaction per request. The contract is that of `signTx`, per transaction.
   */
  signChain?(txs: [BuiltTx, BuiltTx], opts?: SignOptions): Promise<[InputSignature[], InputSignature[]]>;
  /**
   * Tells whether `openSession` can be offered now. That is the private-key signer, once its frame
   * announced sessions. Extension wallets do not define it, because they ask once per transaction.
   */
  canOpenSession?(): boolean;
  /**
   * Shows a whole merge plan (kob/merge.ts), every link as planned, for one approval in the wallet.
   *
   * It resolves with the session that later `signTx` calls name (`SignOptions.session`). It
   * resolves with null when the wallet will not hold one for this plan, and the run then asks per
   * transaction. It rejects with `WalletError` `rejected` when the user rejects it. The wallet
   * checks each step against the plan itself.
   */
  openSession?(plan: BuiltTx[], opts?: { timeoutMs?: number }): Promise<SigningSession | null>;
  /** Ends the session `sid` once the run is over. Any later request is shown for approval. */
  closeSession?(sid: string): void;
  /**
   * Tells whether the wallet ended the session `sid` before its plan was through, on a step off the
   * plan or on expiry. Later steps then ask one by one.
   */
  sessionEnded?(sid: string): boolean;
  /**
   * Re-reads the account, key and network of the already connected wallet without asking the user.
   * There is no connect popup and no network switch. It rejects with `WalletError` when the wallet
   * no longer exposes an account, because it is locked or disconnected the site.
   */
  refresh?(): Promise<WalletInfo>;
  /**
   * Listens for account and network changes of the connected wallet.
   *
   * It uses provider events. When the provider has no emitter, it uses a cheap poll while the tab
   * is visible. Call it after `connect`. It returns an idempotent unsubscribe that removes every
   * listener and timer.
   */
  subscribe?(onEvent: (e: WalletEvent) => void, opts?: WalletSubscribeOptions): () => void;
  /**
   * Hands `tx` to the wallet's own node (KasWare `pushTx`, no popup), so that the wallet finds its
   * outputs at once when it is asked to sign a transaction spending them.
   *
   * It is called only with a transaction KOB finalized, validated and sent to its own node, which
   * is the broadcast that counts. It resolves once that node took it or already had it, and rejects
   * with `WalletError` otherwise.
   */
  announce?(tx: TxJson, opts?: { timeoutMs?: number }): Promise<void>;
  /**
   * Signs a text with the account's key by Kaspa's personal-message standard (BIP340 Schnorr over
   * the BLAKE2b hash keyed `PersonalMessageSigningHash`), as KasWare's `signMessage` and Kaspire's
   * `signMessage` do. No transaction, no fee. A logo vote uses it (data/logo-votes.ts).
   *
   * It resolves with what the wallet returned, which data/logo-votes.ts `normalizeSignature` reads
   * (hex or base64), and rejects with `WalletError` on a refusal or a timeout. A wallet without it
   * cannot vote.
   */
  signMessage?(message: string, opts?: { timeoutMs?: number }): Promise<unknown>;
}

/**
 * A transaction the wallet shows for approval that the app may replace while the user reads it.
 * The private-key signer takes the same request planned again with the clock and the market now,
 * and the frame checks each one itself (signer/core.ts).
 */
export interface LiveSigning {
  /**
   * The wallet hands over what replaces the transaction it shows, and what tells it the clock it
   * says its times from. This holds until it signs.
   */
  attach(to: { replace(built: BuiltTx): void; clock(daa: bigint): void }): void;
  /**
   * Set by the wallet once it signed, to the transaction it signed. That is the one it was given,
   * or a replacement.
   */
  signed?: BuiltTx;
}

/**
 * A one-approval session the wallet opened (`openSession`). `feeMax` and `expiresInMs` are the
 * wallet's own terms, for display.
 */
export interface SigningSession {
  sid: string;
  txs: number;
  feeMax: bigint;
  expiresInMs: number;
}

/** What the app passes with a signing request. */
export interface SignOptions {
  /**
   * Marks a step of the session `sid` (`WalletAdapter.openSession`). It is signed without a prompt
   * while the wallet finds it within the plan.
   */
  session?: string;
  /**
   * Set when the transaction may be replaced while the user reads it. Only a wallet that shows the
   * app's updates takes it, which is the private-key signer.
   */
  live?: LiveSigning;
  /** The network name, used by wallets that need it, such as Kastle. */
  network?: string;
  timeoutMs?: number;
  /**
   * Set when the transaction re-times the one the wallet signed last, planned again with a later
   * clock after the user confirmed it. The private-key signer signs it without asking again when
   * its own decoding finds nothing the user approved got worse. Other wallets ask as usual.
   */
  retime?: boolean;
  /**
   * The node's DAA score now, for display only. It is said to the private-key signer before the
   * request, so that its first view already reads every time as "in 90 d | DAA ..." instead of
   * switching from a bare DAA score once the live clock arrives.
   */
  clockDaa?: bigint;
  /**
   * For `signChain` of a token creation, the states its genesis token outputs are claimed to hold.
   * The private-key signer checks each output's script against them, and only then names the
   * supply and the holders.
   */
  genesisClaim?: GenesisClaim;
  /**
   * The app's non-blocking hints about the order, as codes and numbers. Examples are an order below
   * the minimum value and a price far through the market. The private-key signer words them itself
   * under its table and says they are the app's. Other wallets show nothing of them.
   */
  signerNotes?: SignerNote[];
  /**
   * The wallet's creator locks that the transaction pays a lock coin to (kob/creator-lock.ts). The
   * private-key signer recognises an output to the address of one of its own open locks only with
   * this hint, and checks it itself. Other wallets ignore it.
   */
  locks?: LockHint[];
}

/** A wallet's refusal or failure, with a code the signing pipeline reports. */
export class WalletError extends Error {
  /**
   * `inputs-unknown` means the wallet refused because its own node does not know an input yet, or
   * sees it spent.
   */
  readonly code: 'rejected' | 'timeout' | 'no-signature' | 'unsupported' | 'network' | 'inputs-unknown' | 'other';
  constructor(code: WalletError['code'], message: string) {
    super(message);
    this.name = 'WalletError';
    this.code = code;
  }
}
