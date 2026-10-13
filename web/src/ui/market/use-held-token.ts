// A KCC-20 token that neither the registry nor the indexer lists (no order of it was ever placed): its market page is offered only from the
// wallet's own token UTXOs of it that the node confirms (`kob/open-token.ts` `heldTokenView`). The candidates are the ones this browser stores
// (an issuance made here, transfers built here, an import); the node check is `TokenTracker.tokenUtxosFor`.
import { useMemo } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { heldTokenView, type HeldTokenIssue } from '../../kob/open-token';
import type { Hex } from '../../kob/types';
import { useAsync } from '../kit';
import { unknownRow, type TokenRow } from './token-model';

export interface HeldTokenState {
  row: TokenRow | null;
  /** why the wallet's holdings do not make the token tradable here (null while loading, without a wallet, or with a row) */
  reason: HeldTokenIssue | null;
  loading: boolean;
}

/** `active`: the token is in neither list (the lists have loaded). */
export function useHeldToken(covenantId: Hex, active: boolean): HeldTokenState {
  const { kob, registry, tracker, config } = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  const id = covenantId.toLowerCase();
  const held = useAsync(
    async () => {
      if (!active || !pubkey) return null;
      const programs = [...new Set(tracker.list(pubkey).filter((c) => c.tokenCovId === id).map((c) => c.program))];
      // one program per covenant id: candidates naming two programs are refused below, without asking the node
      const utxos = programs.length === 1 ? await tracker.tokenUtxosFor(pubkey, { covenantId: id, program: programs[0]! }) : [];
      return heldTokenView(kob, id, programs, utxos, config.network);
    },
    [active, pubkey, id, tracker, kob, config.network],
  );
  return useMemo((): HeldTokenState => {
    const r = held.data;
    if (!r) return { row: null, reason: null, loading: active && !!pubkey && held.loading };
    return r.ok ? { row: unknownRow(registry, r.view, 'held'), reason: null, loading: false } : { row: null, reason: r.reason, loading: false };
  }, [held.data, held.loading, registry, active, pubkey]);
}
