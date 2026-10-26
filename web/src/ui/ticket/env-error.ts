// Which source a failed read of the ticket's environment came from, so the ticket names the right one ("Node not answering" for an indexer
// failure or a decimals mismatch sent the user to the wrong place). Pure.
import { IndexerError } from '../../data/indexer';
import { NodeError, isConnectionError } from '../../data/node-error';

export type EnvErrorSource = 'node' | 'indexer' | 'other';

export function envErrorSource(e: unknown): EnvErrorSource {
  if (e instanceof IndexerError) return 'indexer';
  if (e instanceof NodeError || isConnectionError(e)) return 'node';
  return 'other';
}
