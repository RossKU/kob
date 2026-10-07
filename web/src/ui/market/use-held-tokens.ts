// The covenant ids of the tokens the connected wallet holds, from its own token tracker (never from the indexer): the token list compares every
// unregistered token with them for a shared short id, so a token copying one of them is named even when the indexer leaves it out of its list.
import { useMemo } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';

export function useHeldTokenIds(): string[] {
  const { tracker } = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  return useMemo(() => {
    if (!pubkey) return [];
    try {
      return [...new Set(tracker.list(pubkey).map((t) => t.tokenCovId.toLowerCase()))];
    } catch {
      return [];
    }
  }, [tracker, pubkey]);
}
