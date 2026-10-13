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
