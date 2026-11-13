// A token's KCC-23 declaration in a view. It is looked up once (data/token-declarations.ts), from
// the indexer and verified against the covenant id. The view re-renders when any declaration
// becomes known, so the shown name, ticker and decimals follow (app/token-display.ts).
import { useEffect, useMemo, useState } from 'preact/hooks';
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
 * The verified declarations known so far, as a lookup by covenant id. It is a new function each
 * time one becomes known, so it can key a memo.
 */
export function useDeclarationLookup(): (covenantId: Hex) => DeclaredToken | null | undefined {
  const { config } = useServices();
  const v = useDeclarationsVersion();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  return useMemo(() => (covenantId: Hex) => declaredToken(config.network, covenantId), [config.network, v]);
}

/**
 * The verified declaration of `covenantId`: null when it has none, undefined while unknown.
 * The indexer is asked only when `ask` is true, which callers set when the indexer marks the token
 * as declared (`GET /v1/tokens` `declaration`). A token without one then costs no request and no
 * 404 in the console. A declaration known otherwise, such as one kept in this browser or created
 * by it, is returned either way.
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
