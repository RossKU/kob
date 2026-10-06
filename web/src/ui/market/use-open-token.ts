// Loads what the trading of an OPEN-LIST token needs (kob/open-token.ts): the scales of its open orders and, for a KCC-20 token whose indexer row has
// no extension commitment, the commitment of one of its listed orders. Registry tokens are not touched.
import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { synthesizeOpenToken, type ScaleSample, type OpenTokenIssue } from '../../kob/open-token';
import type { TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { useAsync } from '../kit';
import type { TokenRow } from './token-model';

export interface OpenTokenState {
  /** tradable TokenInfo synthesised from the indexer (null for registry tokens and while loading / failed) */
  info: TokenInfo | null;
  /** why the token cannot be traded here (open-list tokens only) */
  reason: OpenTokenIssue | null;
  loading: boolean;
}

const NONE: OpenTokenState = { info: null, reason: null, loading: false };

/** `refreshKey` changes when the book moved: the scales are read again. */
export function useOpenToken(row: TokenRow | null, refreshKey = 0): OpenTokenState {
  const { indexer, kob, registry } = useServices();
  const view = row && row.source !== 'registry' && row.index && row.standing !== 'delisted' ? row.index : null;
  const facts = useAsync(
    async (signal): Promise<{ scales: ScaleSample[]; ext: Hex | null } | null> => {
      if (!view || !indexer) return null;
      const book = await indexer.book(view.covenant_id, { depth: 100, aggregate: false }, { signal });
      const scales = [...book.asks, ...book.bids] as ScaleSample[];
      let ext: Hex | null = view.extension_commitment ?? null;
      if (!ext && view.template_hash) {
        const orders = await indexer.orders({ token: view.covenant_id, status: 'active', limit: 20 }, { signal }).catch(() => null);
        ext = orders?.items.find((o) => o.extension_commitment)?.extension_commitment ?? null;
      }
      return { scales, ext };
    },
    [indexer, view?.covenant_id, view?.template_hash, refreshKey],
  );
  return useMemo((): OpenTokenState => {
    if (!view) return NONE;
    if (!facts.data) return { info: null, reason: facts.error ? 'no-scale' : null, loading: facts.loading };
    const r = synthesizeOpenToken(kob, view, facts.data.scales, facts.data.ext, registry);
    return r.ok ? { info: r.info, reason: null, loading: false } : { info: null, reason: r.reason, loading: false };
  }, [view, facts.data, facts.error, facts.loading, kob, registry]);
}
