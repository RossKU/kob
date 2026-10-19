// A token's KCC-23 declaration in a view: looked up once (data/token-declarations.ts: from the indexer, verified against the covenant id), and
// the view re-renders when any declaration becomes known (the shown name, ticker and decimals follow; app/token-display.ts).
import { useEffect, useState } from 'preact/hooks';
import { useServices } from './context';
import { declaredToken, loadDeclaration, restoreDeclarations, subscribeDeclarations, type DeclaredToken } from '../data/token-declarations';
import type { Hex } from '../kob/types';

/** Re-renders on every newly known declaration; returns a counter to key memos on. */
export function useDeclarationsVersion(): number {
  const [v, setV] = useState(0);
  useEffect(() => subscribeDeclarations(() => setV((n) => n + 1)), []);
  return v;
}

/** The verified declaration of `covenantId` (looked up on first use): null when it has none, undefined while unknown. */
export function useDeclaration(covenantId: Hex | null | undefined): DeclaredToken | null | undefined {
  const { kob, indexer, config } = useServices();
  useDeclarationsVersion();
  useEffect(() => {
    if (!covenantId) return;
    restoreDeclarations(kob);
    void loadDeclaration(kob, indexer, config.network, covenantId);
  }, [kob, indexer, config.network, covenantId]);
  return declaredToken(config.network, covenantId);
}
