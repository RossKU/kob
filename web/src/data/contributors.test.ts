import { describe, expect, it } from 'vitest';
import { CONTRIBUTIONS_MAX_PAGES, CONTRIBUTIONS_PAGE, contributionsUrl, isKaspaAddress, paymentOf, rankContributors, readContributions, type Payment } from './contributors';

// made-up addresses of the right shape: nothing here is a real address
const addr = (c: string) => `kaspa:${c.repeat(61)}`;
const HOME = addr('q');
const A = addr('p');
const B = addr('z');
const txid = (n: number) => n.toString(16).padStart(64, '0');

const input = (address: unknown, amount: unknown = 1_000) => ({ previous_outpoint_address: address, previous_outpoint_amount: amount });
const output = (address: unknown, amount: unknown) => ({ script_public_key_address: address, amount });
function tx(n: number, o: { inputs?: unknown; outputs?: unknown; time?: unknown; accepted?: unknown } = {}) {
  return {
    transaction_id: txid(n),
    block_time: o.time ?? 1_700_000_000_000 + n,
    is_accepted: o.accepted ?? true,
    inputs: o.inputs ?? [input(A)],
    outputs: o.outputs ?? [output(HOME, 500), output(A, 400)],
  };
}

describe('paymentOf', () => {
  it('takes the outputs to the address, all of them, from the sender of the inputs', () => {
    expect(paymentOf(tx(1, { outputs: [output(HOME, 500), output(A, 400), output(HOME, 25)] }), HOME)).toEqual({ txid: txid(1), sender: A, amount: 525n, time: 1_700_000_000_001 });
  });

  it('a payment from several addresses goes to the one that put in the most; a tie to the lower address', () => {
    expect(paymentOf(tx(1, { inputs: [input(A, 10), input(B, 8), input(B, 8)] }), HOME)).toMatchObject({ sender: B });
    expect(paymentOf(tx(1, { inputs: [input(B, 10), input(A, 10)] }), HOME)).toMatchObject({ sender: A });
  });

  it('the address itself never counts: a transaction spending from it, its change included, is no payment', () => {
    expect(paymentOf(tx(1, { inputs: [input(HOME)], outputs: [output(A, 900), output(HOME, 99)] }), HOME)).toBeNull();
    expect(paymentOf(tx(1, { inputs: [input(HOME), input(A)] }), HOME)).toBeNull();
    expect(paymentOf(tx(1, { inputs: [input(HOME), input(null)] }), HOME), 'even beside an unresolved input').toBeNull();
  });

  it('a transaction not accepted, one paying elsewhere and a coinbase are no payments', () => {
    expect(paymentOf(tx(1, { accepted: false }), HOME)).toBeNull();
    expect(paymentOf(tx(1, { outputs: [output(B, 500)] }), HOME)).toBeNull();
    expect(paymentOf(tx(1, { inputs: [] }), HOME)).toBeNull();
    expect(paymentOf({ ...tx(1), inputs: null }, HOME)).toBeNull();
  });

  it('what cannot be told is unreadable, never guessed', () => {
    for (const bad of [
      null,
      'text',
      { ...tx(1), transaction_id: 'x' },
      { ...tx(1), outputs: null },
      tx(1, { outputs: [output(HOME, 2 ** 60)] }), // past 2^53 the JSON number has lost digits
      tx(1, { outputs: [output(HOME, -1)] }),
      tx(1, { inputs: [input(null)] }),
      tx(1, { inputs: [input(A, null)] }),
      tx(1, { time: 'soon' }),
      tx(1, { time: 0 }),
    ]) {
      expect(paymentOf(bad, HOME)).toBe('unreadable');
    }
  });

  it('only addresses reach the page: free text in the place of a sender is not one, and a sender of another network neither', () => {
    for (const text of ['<b>Alice</b>', 'kaspa:visit my site', `${A} `, `${A}\n`, `kaspa:${'b'.repeat(61)}`, `kaspatest:${'p'.repeat(61)}`]) {
      expect(paymentOf(tx(1, { inputs: [input(text)] }), HOME), text).toBe('unreadable');
    }
    const memo = { ...tx(1), payload: '48656c6c6f', name: 'Alice' };
    expect(Object.keys(paymentOf(memo, HOME) as Payment).sort()).toEqual(['amount', 'sender', 'time', 'txid']);
  });

  it('isKaspaAddress: both networks, the three lengths of the format', () => {
    for (const a of [addr('q'), `kaspa:${'q'.repeat(63)}`, `kaspatest:${'q'.repeat(61)}`]) expect(isKaspaAddress(a), a).toBe(true);
    for (const a of ['', 'kaspa:', `kaspa:${'q'.repeat(60)}`, `kaspa:${'q'.repeat(64)}`, `kaspadev:${'q'.repeat(61)}`, 7, null]) expect(isKaspaAddress(a), String(a)).toBe(false);
  });
});

describe('rankContributors', () => {
  const p = (sender: string, amount: bigint, time: number, n = time): Payment => ({ txid: txid(n), sender, amount, time });

  it('sums per sender and keeps the first time; the larger total comes first', () => {
    expect(rankContributors([p(A, 5n, 30), p(B, 9n, 20), p(A, 6n, 10)])).toEqual([
      { address: A, total: 11n, first: 10 },
      { address: B, total: 9n, first: 20 },
    ]);
  });

  it('a tie goes to the earlier first contribution, then to the lower address', () => {
    expect(rankContributors([p(A, 5n, 20), p(B, 5n, 10)]).map((c) => c.address)).toEqual([B, A]);
    expect(rankContributors([p(B, 5n, 10, 1), p(A, 5n, 10, 2)]).map((c) => c.address)).toEqual([A, B]);
  });

  it('compares totals as integers, past what a number holds exactly', () => {
    const big = 2n ** 60n;
    expect(rankContributors([p(A, big, 1), p(B, big + 1n, 2)]).map((c) => c.address)).toEqual([B, A]);
  });
});

describe('readContributions', () => {
  type Page = { rows: unknown; next?: string };
  function explorer(pages: (Page | Error | number)[]) {
    const urls: string[] = [];
    const fetchFn = (async (url: string) => {
      urls.push(url);
      const page = pages[Math.min(urls.length - 1, pages.length - 1)]!;
      if (page instanceof Error) throw page;
      if (typeof page === 'number') return new Response('{}', { status: page });
      return new Response(JSON.stringify(page.rows), { status: 200, headers: page.next ? { 'x-next-page-before': page.next } : {} });
    }) as typeof fetch;
    return { urls, fetch: fetchFn };
  }

  it("asks the address's network, for the fields it uses and no payload", () => {
    const url = contributionsUrl(HOME, null);
    expect(url).toBe(`https://api.kaspa.org/addresses/${HOME}/full-transactions-page?limit=${CONTRIBUTIONS_PAGE}&resolve_previous_outpoints=light&acceptance=accepted&fields=transaction_id,block_time,is_accepted,inputs,outputs`);
    expect(contributionsUrl(HOME, '123').endsWith('&before=123')).toBe(true);
    expect(contributionsUrl(`kaspatest:${'q'.repeat(61)}`, null).startsWith('https://api-tn10.kaspa.org/addresses/kaspatest:')).toBe(true);
    expect(CONTRIBUTIONS_PAGE).toBeLessThanOrEqual(500);
  });

  it('follows the cursor to the last page; a transaction seen twice counts once', async () => {
    const e = explorer([{ rows: [tx(3), tx(2)], next: '1700000000002' }, { rows: [tx(2), tx(1, { inputs: [input(B)] })] }]);
    const r = await readContributions(HOME, e);
    expect(e.urls).toHaveLength(2);
    expect(e.urls[1]).toContain('&before=1700000000002');
    expect(r).toEqual({
      contributors: [
        { address: A, total: 1000n, first: 1_700_000_000_002 },
        { address: B, total: 500n, first: 1_700_000_000_001 },
      ],
      complete: true,
    });
  });

  it('no transactions: an empty table, complete', async () => {
    expect(await readContributions(HOME, explorer([{ rows: [] }]))).toEqual({ contributors: [], complete: true });
  });

  it('stops at the cap and says the history is not complete', async () => {
    const e = explorer([{ rows: [tx(1)], next: '5' }]);
    const r = await readContributions(HOME, e);
    expect(e.urls).toHaveLength(CONTRIBUTIONS_MAX_PAGES);
    expect(r.complete).toBe(false);
    expect(r.contributors).toHaveLength(1);
  });

  it('a transaction that cannot be read leaves the others and says so', async () => {
    const r = await readContributions(HOME, explorer([{ rows: [tx(1), tx(2, { inputs: [input(null)] })] }]));
    expect(r.contributors).toHaveLength(1);
    expect(r.complete).toBe(false);
  });

  it('any failed request fails the read: no table of half the history', async () => {
    await expect(readContributions(HOME, explorer([429]))).rejects.toThrow('HTTP 429');
    await expect(readContributions(HOME, explorer([new Error('offline')]))).rejects.toThrow('offline');
    await expect(readContributions(HOME, explorer([{ rows: [tx(1)], next: '5' }, 500]))).rejects.toThrow('HTTP 500');
    await expect(readContributions(HOME, explorer([{ rows: { detail: 'nope' } }]))).rejects.toThrow('not a list');
    await expect(readContributions(HOME, explorer([{ rows: [], next: 'soon' }]))).rejects.toThrow('bad cursor');
  });

  it('stops when the page goes away', async () => {
    const ctl = new AbortController();
    const fetchFn = ((_: string, init: RequestInit) =>
      new Promise((_resolve, reject) => {
        init.signal!.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
        ctl.abort();
      })) as typeof fetch;
    await expect(readContributions(HOME, { fetch: fetchFn, signal: ctl.signal })).rejects.toThrow('aborted');
  });
});
