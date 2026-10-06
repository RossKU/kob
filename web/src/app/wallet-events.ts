// How the app reacts to a wallet that changed account or network AFTER connect. Pure (no DOM, no Preact): the WalletProvider feeds it the
// re-read `WalletInfo` and applies the outcome; tests drive it directly.
import type { WalletAdapter, WalletInfo } from '../wallet/types';

export type WalletNoticeKind =
  /** the wallet switched to another account (another key): everything bound to the old key is stale */
  | 'account-changed'
  /** the wallet moved to a network other than the app's (trading is blocked by the mismatch banner) */
  | 'network-changed'
  /** the wallet is back on the app's network */
  | 'network-restored'
  /** the wallet no longer exposes an account (locked / site disconnected): the session was closed */
  | 'wallet-lost';

export interface WalletNotice {
  kind: WalletNoticeKind;
  /** wallet name */
  wallet: string;
  /** address before / after (account changes) and networks before / after (network changes) */
  from?: { address: string; network: string };
  to?: { address: string; network: string };
}

export interface WalletChange {
  /** the session after the change (`null` = disconnected) */
  info: WalletInfo | null;
  notice: WalletNotice | null;
  /** something a plan / confirmation could be bound to changed (key or network): views must drop what they hold for the old identity */
  identityChanged: boolean;
}

/** Same account on the same network (the version string is not part of the identity). */
export const sameWallet = (a: WalletInfo, b: WalletInfo): boolean => a.id === b.id && a.pubkey === b.pubkey && a.address === b.address && a.network === b.network;

/**
 * `prev` is the session the app holds, `next` what the wallet reports now (`null`: it reports no account). Events can repeat and a wallet
 * echoes our own calls back, so an unchanged wallet yields `prev` itself (no re-render, no notice).
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
  // same key and network, another display address (the wallet re-encoded it): keep the newer string, nothing is stale
  return { info: next, notice: null, identityChanged: false };
}

export interface WatchWalletOptions {
  adapter: WalletAdapter;
  /** the session the app holds right now (read at every event) */
  current: () => WalletInfo | null;
  appNetwork: string;
  /** called for every real change (never for an echo of the current state) */
  onChange: (change: WalletChange) => void;
  /** wait before the single retry of a timed-out re-read, ms (default 500) */
  retryMs?: number;
}

/**
 * Subscribes to the adapter's account / network events for as long as the returned function has not been called, and answers each event
 * by re-reading the wallet. Events that arrive while a re-read runs supersede it (the newest read wins). A wallet that cannot be read
 * (no account any more) closes the session. Every listener is removed by the returned function; nothing fires after it.
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
      // a timeout is retried once (a busy wallet); anything else means the account is gone
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
