// KOB's own genesis check of the token a page shows (data/genesis-check.ts), run when the page opens: once per token per session (the checker's cache).
// A row whose genesis the registry verified has nothing to check. The result never gates anything: it replaces the genesis fact of the row
// (token-model.ts `applyGenesisCheck`): "Genesis verified" (KOB's own fact), "Genesis check failed", or "Genesis not verified" with the reason.
import { useEffect, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { Services } from '../../app/services';
import { createGenesisChecker, type GenesisCheck, type GenesisChecker } from '../../data/genesis-check';
import { kascovNetwork } from '../../data/kascov';
import { verifyGenesis } from '../../kob/genesis';
import type { TokenRow } from './token-model';

const checkers = new WeakMap<object, GenesisChecker>();

/** The session's checker for a services object (the wasm facade it verifies with). */
export function genesisCheckerFor(services: Pick<Services, 'kob'>): GenesisChecker {
  let c = checkers.get(services.kob);
  if (!c) {
    c = createGenesisChecker({ verify: (req) => verifyGenesis(services.kob, req) });
    checkers.set(services.kob, c);
  }
  return c;
}

/** Whether the row's genesis line is open (not verified by the registry): only those tokens are checked. */
export const needsGenesisCheck = (row: Pick<TokenRow, 'facts'> | null | undefined): boolean =>
  !!row && (row.facts.includes('genesis_unverified') || row.facts.includes('genesis_kascov'));

export function useGenesisCheck(row: Pick<TokenRow, 'covenantId' | 'facts' | 'index' | 'info'> | null): GenesisCheck | null {
  const services = useServices();
  const id = row?.covenantId.toLowerCase() ?? null;
  const enabled = id !== null && needsGenesisCheck(row) && kascovNetwork(services.config.network) !== null;
  const checker = genesisCheckerFor(services);
  const hint = row?.info?.templateHash || row?.index?.template_hash || null;
  const [result, setResult] = useState<{ id: string; check: GenesisCheck } | null>(() => {
    const known = enabled && id ? checker.known(services.config.network ?? '', id) : null;
    return known && id ? { id, check: known } : null;
  });
  const base = services.config.tokenNamesUrl;

  useEffect(() => {
    if (!enabled || !id) return;
    // the request is not cancelled when the page is left: it is bounded by its timeouts and its answer is the session's cache for the token
    let live = true;
    void checker.check(services.config.network ?? '', id, { kascovBase: base, programHint: hint }).then((check) => {
      if (live) setResult({ id, check });
    });
    return () => {
      live = false;
    };
  }, [enabled, id, base, checker, services.config.network, hint]);

  return enabled && result && result.id === id ? result.check : null;
}
