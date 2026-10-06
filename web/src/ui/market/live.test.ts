import { describe, expect, it } from 'vitest';
import { liveMode } from './live';

describe('liveMode', () => {
  it('is off without an indexer, live only while the socket is open, polling otherwise', () => {
    expect(liveMode(false, 'open')).toBe('off');
    expect(liveMode(true, 'open')).toBe('live');
    for (const s of ['idle', 'connecting', 'reconnecting', 'closed', null] as const) expect(liveMode(true, s)).toBe('polling');
  });
});
