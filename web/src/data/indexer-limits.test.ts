// The indexer client reads at most `maxBodyBytes` of a response, and the indexer's error text reaches the UI without control, bidi or zero-width
// characters.
import { describe, expect, it } from 'vitest';
import { HttpIndexer, IndexerError } from './indexer';
import { hasBadChar, plainUntrusted } from '../kob/registry';

const fetchOf = (make: () => Response): typeof fetch => (async () => make()) as unknown as typeof fetch;
const json = (v: unknown, status = 200, headers: Record<string, string> = {}) => new Response(JSON.stringify(v), { status, headers: { 'content-type': 'application/json', ...headers } });

describe('indexer response size cap', () => {
  it('a body within the cap is read', async () => {
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: fetchOf(() => json({ tokens: [] })), maxBodyBytes: 1024 });
    expect(await ix.tokens()).toEqual([]);
  });

  it('a body over the cap is refused, whether it declares its length or streams', async () => {
    const big = { tokens: [], pad: 'x'.repeat(4096) };
    const declared = new HttpIndexer({ baseUrl: 'http://x', fetch: fetchOf(() => json(big, 200, { 'content-length': '5000' })), maxBodyBytes: 1024 });
    await expect(declared.tokens()).rejects.toMatchObject({ code: 'too_large', kind: 'parse' });
    const streamed = () => {
      const bytes = new TextEncoder().encode(JSON.stringify(big));
      const body = new ReadableStream<Uint8Array>({
        start(c) {
          for (let i = 0; i < bytes.length; i += 256) c.enqueue(bytes.slice(i, i + 256));
          c.close();
        },
      });
      return new Response(body, { status: 200 });
    };
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: fetchOf(streamed), maxBodyBytes: 1024 });
    const e = await ix.tokens().catch((x) => x);
    expect(e).toBeInstanceOf(IndexerError);
    expect((e as IndexerError).code).toBe('too_large');
  });
});

const ch = (...codes: number[]): string => String.fromCharCode(...codes);
const RLO = ch(0x202e);
const PDF = ch(0x202c);
const ZWSP = ch(0x200b);

describe('indexer error text', () => {
  it('bidi, zero-width and control characters are removed and the text is cut', async () => {
    const message = `Order ${RLO}exe.txt${PDF} not${ZWSP} found${ch(7)}${ch(10)}please   retry ` + 'y'.repeat(400);
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: fetchOf(() => json({ error: { code: `not_found${RLO}`, message } }, 400)) });
    const e = (await ix.tokens().catch((x) => x)) as IndexerError;
    expect(e).toBeInstanceOf(IndexerError);
    expect(e.code).toBe('http_400');
    expect(e.message.startsWith('Order exe.txt not found please retry y')).toBe(true);
    expect(hasBadChar(e.message)).toBe(false);
    expect([...e.message].length).toBeLessThanOrEqual(301);
  });

  it('an error message that is nothing but hidden characters falls back to the status', async () => {
    const ix = new HttpIndexer({ baseUrl: 'http://x', fetch: fetchOf(() => json({ error: { code: 'busy', message: ZWSP + RLO } }, 400)) });
    const e = (await ix.tokens().catch((x) => x)) as IndexerError;
    expect(e.code).toBe('busy');
    expect(e.message).toBe('The indexer answered HTTP 400.');
    expect(plainUntrusted(`a${ch(0x2066)}b${ch(0x2069)}c`)).toBe('abc');
  });
});
