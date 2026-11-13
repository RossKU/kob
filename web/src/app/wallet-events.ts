// How the app reacts to a wallet that changed account or network after connect. It is pure, with
// no DOM and no Preact. The WalletProvider feeds it the re-read `WalletInfo` and applies the
// outcome, and tests drive it directly.
import type { WalletAdapter, WalletInfo } from '../wallet/types';

/** The kinds of wallet change the app tells the user about. */
export type WalletNoticeKind =
  /** The wallet switched to another account, so what is bound to the old key is stale. */
  | 'account-changed'
  /**
   * The wallet moved to a network other than the app's. The views block trading on
   * `networkMismatch`, and the mismatch banner says so.
   */
  | 'network-changed'
  /** The wallet is back on the app's network. */
  | 'network-restored'
  /** The wallet no longer exposes an account (locked or disconnected). The session closed. */
  | 'wallet-lost';

/** A notice for the user about a wallet change. */
export interface WalletNotice {
  kind: WalletNoticeKind;
  /** The wallet name. */
  wallet: string;
  /** Address before and after (account changes), network before and after (network changes). */
  from?: { address: string; network: string };
  to?: { address: string; network: string };
}

/** The outcome of a wallet change. */
export interface WalletChange {
  /** The session after the change. Null means disconnected. */
  info: WalletInfo | null;
  notice: WalletNotice | null;
  /**
   * Whether something a plan or confirmation could be bound to changed, which is the key or the
   * network. Views must drop what they hold for the old identity.
   */
  identityChanged: boolean;
}

/** Whether two sessions are the same account on the same network. The version is not identity. */
export const sameWallet = (a: WalletInfo, b: WalletInfo): boolean => a.id === b.id && a.pubkey === b.pubkey && a.address === b.address && a.network === b.network;

/**
 * Compares `prev`, the session the app holds, with `next`, what the wallet reports now (null when
 * it reports no account). Events can repeat and a wallet echoes our own calls back, so an
 * unchanged wallet yields `prev` itself, with no re-render and no notice.
 */
export function reactToWalletChange(prev: WalletInfo, next: WalletInfo | null, appNetwork: string): WalletChange {
  if (next === null) return { info: null, notice: { kind: 'wallet-lost', wallet: prev.label }, identityChanged: true };
  if (sameWallet(prev, next)) return { info: prev, notice: null, identityChanged: false };
  const from = { address: prev.address, network: prev.network };
  const to = { address: next.address, network: next.network };
  if (prev.pubkey !== next.pubkey) return { info: next, notice: { kind: 'account-changed', wallet: next.label, from, to }, identityChanged: true };
  if (prev.network !== next.network) {
    const kind: WalletNoticeKind = next.network === appNetwork && prev.network !== appNetwork ? 'network-restored' : 'network-changed';
    return { info: next, notice: { kind, wallet: next.label, from, to }, identityChanged: true };
  }
  // Same key and network, another display address because the wallet re-encoded it. Keep the newer
  // string. Nothing is stale.
  return { info: next, notice: null, identityChanged: false };
}

/** What `watchWallet` needs. */
export interface WatchWalletOptions {
  adapter: WalletAdapter;
  /** The session the app holds right now. Read at every event. */
  current: () => WalletInfo | null;
  appNetwork: string;
  /** Called for every real change, never for an echo of the current state. */
  onChange: (change: WalletChange) => void;
  /** Wait in ms before the single retry of a timed-out re-read. Default 500. */
  retryMs?: number;
}

/**
 * Subscribes to the adapter's account and network events until the returned function is called,
 * and answers each event by re-reading the wallet. Events that arrive while a re-read runs
 * supersede it, so the newest read wins. A wallet that cannot be read, because it has no account
 * any more, closes the session. The returned function removes every listener, and nothing fires
 * after it.
 */
export function watchWallet(o: WatchWalletOptions): () => void {
  const { adapter } = o;
  if (!adapter.subscribe || !adapter.refresh) return () => undefined;
  let dead = false;
  let gen = 0;
  const apply = (next: WalletInfo | null) => {
    const prev = o.current();
    if (dead || !prev) return;
    const change = reactToWalletChange(prev, next, o.appNetwork);
    if (change.info === prev && !change.notice) return;
    o.onChange(change);
  };
  const reread = async (attempt: number, my: number): Promise<void> => {
    let next: WalletInfo | null;
    try {
      next = await adapter.refresh!();
    } catch (e) {
      // A timeout is retried once, since a busy wallet may answer. Anything else means the
      // account is gone.
      if (attempt === 0 && (e as { code?: string } | null)?.code === 'timeout') {
        await new Promise((r) => setTimeout(r, o.retryMs ?? 500));
        if (dead || my !== gen) return;
        return reread(1, my);
      }
      next = null;
    }
    if (!dead && my === gen) apply(next);
  };
  const unsubscribe = adapter.subscribe(() => void reread(0, ++gen));
  return () => {
    if (dead) return;
    dead = true;
    unsubscribe();
  };
}
