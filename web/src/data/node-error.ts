// Errors of node access, shared by the wRPC node and the HTTP mock node. The SDK rejects with a bare Error / string whose text looks like
//   RPC Server (remote error) -> code:0  message:`Rejected transaction <id>: <reason>` data:None
// which is unreadable in a toast: `describeNodeError` maps the known consensus / mempool rejections to a stable code plus a short message.

export type NodeErrorCode =
  | 'unavailable'      // no connection / socket dropped / timeout
  | 'network-mismatch' // node is on another network than the app is configured for
  | 'orphan'           // an input is unknown to the node: already spent, not yet visible, or never existed
  | 'double-spend'     // an input is already spent by another transaction in the mempool
  | 'already-known'    // the very same transaction is already in the mempool
  | 'fee'              // fee below the relay minimum (or mass over the limit)
  | 'script'           // a script failed: bad signature or a covenant rule
  | 'invalid'          // any other consensus / policy rejection
  | 'bad-response'     // the server answered something we do not understand
  | 'other';

export class NodeError extends Error {
  readonly code: NodeErrorCode;
  /** raw text as received (for logs / a "details" disclosure) */
  readonly raw: string;
  constructor(code: NodeErrorCode, message: string, raw = message) {
    super(message);
    this.name = 'NodeError';
    this.code = code;
    this.raw = raw;
  }
}

/** Text of whatever the SDK / fetch threw. */
export function rawErrorText(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === 'object') {
    const m = (e as { message?: unknown }).message;
    if (typeof m === 'string') return m;
    try {
      return JSON.stringify(e);
    } catch {
      /* fall through */
    }
  }
  return String(e);
}

const CONNECTION = /websocket|not connected|disconnect|connection (?:closed|refused|reset|lost)|refused|timed? ?out|network error|failed to fetch|econn|enotfound|socket|unreachable/i;

/** True for errors that mean "the link is down" (worth waiting for a reconnect and retrying an idempotent read). */
export const isConnectionError = (e: unknown): boolean => e instanceof NodeError ? e.code === 'unavailable' : CONNECTION.test(rawErrorText(e));

/** Extracts the node's own reason from the SDK wrapper text. */
export function nodeReason(raw: string): string {
  let s = raw.replace(/^Error:\s*/, '');
  const m = /message:`([\s\S]*?)`\s*data:/.exec(s);
  if (m) s = m[1] ?? s;
  s = s.replace(/^RPC Server \(remote error\) ->\s*/, '');
  // "Rejected transaction <id>: <reason>" -> "<reason>"; the id is not useful to a user
  s = s.replace(/^Rejected transaction [0-9a-f]+:\s*/i, '').replace(/transaction [0-9a-f]{64}\s+/gi, 'transaction ');
  return s.trim();
}

/**
 * Maps a raw failure to a `NodeError`. Idempotent for NodeError inputs. `context` 'read' (UTXO / info queries) skips the
 * transaction-rejection classes, which would read wrongly for a failed query.
 */
export function describeNodeError(e: unknown, context: 'submit' | 'read' = 'submit'): NodeError {
  if (e instanceof NodeError) return e;
  const raw = rawErrorText(e);
  const reason = nodeReason(raw);
  const low = reason.toLowerCase();
  if (context === 'read') {
    return CONNECTION.test(raw)
      ? new NodeError('unavailable', 'The node connection was lost. Check your connection and try again.', raw)
      : new NodeError('other', reason || 'Unknown node error', raw);
  }
  if (/orphan/.test(low)) {
    return new NodeError('orphan', 'The node does not know one of the inputs (already spent, not yet confirmed, or wrong network). Refresh and try again.', raw);
  }
  if (/already spent|double spend|spent by (?:another|transaction)|missing[- ]?outpoint/.test(low) && !/orphan/.test(low)) {
    return new NodeError('double-spend', 'An input of this transaction is already being spent by another transaction.', raw);
  }
  if (/already (?:in|exists in) (?:the )?mempool|already accepted|duplicate transaction|transaction is already in the mempool/.test(low)) {
    return new NodeError('already-known', 'The node already has this transaction.', raw);
  }
  if (/fee|mass|dust/.test(low) && /(?:insufficient|too low|below|minimum|exceed|too (?:high|large)|not enough|dust|limit)/.test(low)) {
    return new NodeError('fee', `The node rejected the transaction fee or size: ${reason}`, raw);
  }
  if (/script|signature|verification failed|evaluated to false|covenant|checksig|stack/.test(low)) {
    return new NodeError('script', `A script or covenant rule failed: ${reason}`, raw);
  }
  if (CONNECTION.test(raw)) return new NodeError('unavailable', 'The node connection was lost. Check your connection and try again.', raw);
  if (/rejected|invalid|not allowed|is not standard|non-standard/.test(low)) return new NodeError('invalid', `The node rejected the transaction: ${reason}`, raw);
  return new NodeError('other', reason || 'Unknown node error', raw);
}
