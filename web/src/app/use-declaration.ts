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

/**
 * The verified declaration of `covenantId`: null when it has none, undefined while unknown. It is asked of the indexer only when `ask` (the
 * indexer marks the token as declared: `GET /v1/tokens` `declaration`), so a token without one costs no request (and no 404 in the console);
 * one known otherwise (kept in this browser, this browser's own creation) is returned either way.
 */
export function useDeclaration(covenantId: Hex | null | undefined, ask = true): DeclaredToken | null | undefined {
  const { kob, indexer, config } = useServices();
  useDeclarationsVersion();
  useEffect(() => {
    if (!covenantId) return;
    restoreDeclarations(kob);
    if (ask) void loadDeclaration(kob, indexer, config.network, covenantId);
  }, [kob, indexer, config.network, covenantId, ask]);
  return declaredToken(config.network, covenantId);
}
