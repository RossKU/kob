// The connected wallet's node-verified token outputs, for the market list (held-list.ts). They come
// from the source the Balances panel reads (`TokenTracker.heldTokens`: the indexer's holdings of
// the key and this browser's candidates, each confirmed on the node), so the list adds no fetch
// path of its own. Without a wallet, or with one on another network, there are none: the rows they
// add are only ever in the connected wallet's own view.
import { useEffect } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { loadDeclaration, noteTokenProgram } from '../../data/token-declarations';
import type { HeldTokenUtxo } from '../../data/token-tracker';
import { useAsync } from '../kit';

export interface WalletHoldings {
  /** null without a connected wallet; empty while it is read or when it could not be */
  holdings: readonly HeldTokenUtxo[] | null;
  /** a wallet is connected on this network */
  connected: boolean;
}

const NONE: readonly HeldTokenUtxo[] = [];

/** `reloadKey`: a new value reads the holdings again (the list's refresh). */
export function useWalletHoldings(reloadKey: unknown): WalletHoldings {
  const { tracker, config } = useServices();
  const wallet = useWallet();
  const pubkey = wallet.networkMismatch ? null : (wallet.info?.pubkey ?? null);
  // A failed read leaves the list as the indexer gives it: the holdings only add to it. Each answer
  // names its key, so the last answer of a wallet that was switched away from is never shown as the
  // new one's while that one is read.
  const read = useAsync(
    async (_signal) => (pubkey && tracker ? { pubkey, list: await tracker.heldTokens(pubkey).catch((): HeldTokenUtxo[] => []) } : null),
    [pubkey, tracker, reloadKey],
  );
  const mine = read.data && read.data.pubkey === pubkey ? read.data.list : null;
  // The program each held token verified under: a token without a registry entry is then shown with
  // its program's default decimals elsewhere in the app, as the Balances panel notes them
  // (balances-data.ts).
  useEffect(() => {
    for (const h of mine ?? []) noteTokenProgram(config.network, h.covenantId, h.program);
  }, [mine, config.network]);
  return { holdings: pubkey ? (mine ?? NONE) : null, connected: !!pubkey };
}

/**
 * Asks the indexer once for the KCC-23 declaration of each of `ids` (verified before anything of it
 * is shown, data/token-declarations.ts), as the token page does for a token only the wallet's
 * holdings show. The list re-renders when one becomes known (`useDeclarationLookup`).
 */
export function useHeldDeclarations(ids: readonly string[]): void {
  const { kob, indexer, config } = useServices();
  const key = ids.join(',');
  useEffect(() => {
    for (const id of ids) void loadDeclaration(kob, indexer, config.network, id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, kob, indexer, config.network]);
}
