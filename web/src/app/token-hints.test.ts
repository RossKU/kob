// The signer frame's token labels name the token of a buy too (TN10 2026-10-14 B-7: a KobBid holds none of its token, so the transaction's
// token inputs and outputs did not name it and the frame showed "Token 55aac2b5…773e07bf", "Minimum fill ... base units (unknown token)").
import { describe, expect, it } from 'vitest';
import type { TokenInfo, TokenRegistry } from '../kob/registry';
import { goldenRequest } from '../testing/golden';
import { MAKER_PK, TOKEN_COV_ID } from '../testing/token-fixtures';
import { kob } from '../testing/wallet-fixtures';
import { summarize } from '../signer/summary';
import { signerTokenHints } from './token-hints';

const registry: TokenRegistry = {
  network: '', templates: [], templateChecks: [], tokens: [],
  byCovenantId: new Map([[TOKEN_COV_ID, { covenantId: TOKEN_COV_ID, ticker: 'EXKCC', decimals: 3 } as unknown as TokenInfo]]),
};

describe('signerTokenHints', () => {
  it("names a buy's token (no token moves) and a sell's; without the maker only the moved tokens", () => {
    const bid = kob.build(goldenRequest('create.bid', MAKER_PK));
    expect(bid.tx.outputs.some((o) => o.covenant?.covenantId === TOKEN_COV_ID)).toBe(false);
    const hints = signerTokenHints(kob, bid, MAKER_PK, registry);
    expect(hints).toEqual([{ covenantId: TOKEN_COV_ID, ticker: 'EXKCC', decimals: 3 }]);
    // the frame then names the token and its minimum fill in tokens
    const s = summarize(kob, bid, MAKER_PK, hints);
    expect(s.orders[0]!.token).toContain('EXKCC');
    expect(s.orders[0]!.rows.map((r) => r.value).join(' ')).not.toContain('unknown token');
    expect(signerTokenHints(kob, bid, null, registry)).toEqual([]);
    const ask = kob.build(goldenRequest('create.ask', MAKER_PK));
    expect(signerTokenHints(kob, ask, null, registry).map((h) => h.ticker)).toEqual(['EXKCC']);
  });
});

describe('signerTokenHints: a token the registry does not list (founder 2026-10-17: never "base units")', () => {
  const empty: TokenRegistry = { network: '', templates: [], templateChecks: [], tokens: [], byCovenantId: new Map() };
  // the page's localStorage (the test runs in node): what the issuing browser remembered
  const m = new Map<string, string>();
  const g = globalThis as unknown as { localStorage?: unknown };
  g.localStorage = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k), clear: () => m.clear() };
  const store = (entries: Record<string, unknown>) => m.set('kob.issuedDecimals.v1', JSON.stringify({ 'testnet-10': entries }));

  it('hints the issuing browser\'s ticker and decimals (unverified), else the assumed 8 decimals without a ticker; never an order covenant', () => {
    const ask = kob.build(goldenRequest('create.ask', MAKER_PK));
    m.clear();
    const assumed = signerTokenHints(kob, ask, MAKER_PK, empty, 'testnet-10');
    expect(assumed).toEqual([{ covenantId: TOKEN_COV_ID, ticker: '', decimals: 8, source: 'assumed' }]);
    // the frame names it by its covenant id, marked, and its amounts are whole tokens
    const a = summarize(kob, ask, MAKER_PK, assumed);
    expect(a.orders[0]!.token).toMatch(/^[0-9a-f]{8}…[0-9a-f]{8} \(decimals assumed 8\)$/);
    expect(a.orders[0]!.rows.map((r) => r.value).join(' ')).not.toMatch(/base units/);
    store({ [TOKEN_COV_ID]: { decimals: 3, ticker: 'XYZ', name: 'Xyz' } });
    const issued = signerTokenHints(kob, ask, MAKER_PK, empty, 'testnet-10');
    expect(issued).toEqual([{ covenantId: TOKEN_COV_ID, ticker: 'XYZ', decimals: 3, source: 'issued' }]);
    expect(summarize(kob, ask, MAKER_PK, issued).orders[0]!.token).toMatch(/^XYZ \([0-9a-f]{8}…[0-9a-f]{8}, unverified\)$/);
    // a registry entry wins over the issuing browser's record
    expect(signerTokenHints(kob, ask, MAKER_PK, registry, 'testnet-10')).toEqual([{ covenantId: TOKEN_COV_ID, ticker: 'EXKCC', decimals: 3 }]);
    m.clear();
  });
});
