import { describe, expect, it } from 'vitest';
import { IndexerError } from '../../data/indexer';
import { NodeError } from '../../data/node-error';
import { envErrorSource } from './env-error';

describe('envErrorSource', () => {
  it('names the source that failed', () => {
    expect(envErrorSource(new IndexerError('network', 0, 'network', 'The indexer did not answer.'))).toBe('indexer');
    expect(envErrorSource(new NodeError('unavailable', 'The node connection was lost.'))).toBe('node');
    expect(envErrorSource(new Error('WebSocket is not connected'))).toBe('node');
    expect(envErrorSource('connection refused')).toBe('node');
  });
  it('does not blame the node for anything else (a token whose decimals disagree, a bug)', () => {
    expect(envErrorSource(new Error('token FOO: its registry entry says 8 decimals, what it recorded on chain says 6; orders wait until they agree'))).toBe('other');
    expect(envErrorSource(undefined)).toBe('other');
  });
});
