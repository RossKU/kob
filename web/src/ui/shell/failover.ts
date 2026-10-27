// The indexer failover state for the views (data/indexer-failover.ts): a component that shows what depends on WHICH indexer is in use (the banner, the
// trust line, the cross-check) reads it here and re-renders on a switch.
import { useEffect, useState } from 'preact/hooks';
import type { FailoverState, IndexerFailover } from '../../data/indexer-failover';

/** The failover state now (null: no failover, a single indexer), kept current. */
export function useFailoverState(failover: IndexerFailover | null | undefined): FailoverState | null {
  const [state, setState] = useState<FailoverState | null>(failover ? failover.state : null);
  useEffect(() => {
    if (!failover) return setState(null);
    setState(failover.state);
    return failover.subscribe(setState);
  }, [failover]);
  return state;
}
