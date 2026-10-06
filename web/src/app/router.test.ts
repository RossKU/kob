import { describe, expect, it } from 'vitest';
import { navOf, parseHash, routeToHash, tokenRoute, type Route, pairRoute, usdMarketHash, usdMarketRoute } from './router';

const ID = 'ab'.repeat(32);

describe('router', () => {
  it('an empty hash is the landing screen, an unknown one the market list', () => {
    for (const h of ['', '#', '#/', '#///']) expect(parseHash(h)).toEqual({ name: 'home' });
    for (const h of ['#/nope', '#/market', '#/market/', '#/unknown/page']) expect(parseHash(h)).toEqual({ name: 'market' });
    expect(routeToHash({ name: 'home' })).toBe('#/');
    expect(navOf({ name: 'home' })).toBe('market');
  });

  it('old USD links still parse (the app redirects them), and map to the tradable pages', () => {
    expect(parseHash('#/usd/kas')).toEqual({ name: 'usd', asset: 'kas' });
    expect(parseHash('#/usd/KAS')).toEqual({ name: 'usd', asset: 'kas' });
    expect(parseHash(`#/usd/${ID.toUpperCase()}`)).toEqual({ name: 'usd', asset: ID });
    expect(parseHash('#/usd/nope')).toMatchObject({ name: 'market', invalidToken: 'nope' });
    const kas: Route = { name: 'usd', asset: 'kas' };
    expect(routeToHash({ name: 'usd', asset: ID })).toBe(`#/usd/${ID}`);
    expect(parseHash(routeToHash(kas))).toEqual(kas);
    expect(navOf(kas)).toBe('market');
    // KAS/USD -> the USD token's own market (book, trades, ticket); a token in USD -> the pair page <token>/<USD token>
    const USD = 'cd'.repeat(32);
    expect(usdMarketHash('kas', USD)).toBe(`#/market/${USD}`);
    expect(usdMarketHash(USD.toUpperCase(), USD)).toBe(`#/market/${USD}`);
    expect(usdMarketHash(ID.toUpperCase(), USD)).toBe(`#/market/${ID}/${USD}`);
    // the targets are real, tradable pages
    expect(usdMarketRoute(ID, USD).name).toBe('pair');
    expect(usdMarketRoute('kas', USD).name).toBe('token');
    expect(parseHash(usdMarketHash(ID, USD))).toEqual(pairRoute(ID, USD));
    expect(parseHash(usdMarketHash('kas', USD))).toEqual(tokenRoute(USD));
  });

  it('parses the pages', () => {
    expect(parseHash('#/orders')).toEqual({ name: 'orders' });
    expect(parseHash('#/issue')).toEqual({ name: 'issue' });
    expect(parseHash('#/settings')).toEqual({ name: 'settings' });
    expect(parseHash('/orders')).toEqual({ name: 'orders' });
    expect(parseHash('#/ORDERS/')).toEqual({ name: 'orders' });
  });

  it('parses a token page and normalises the id to lower case', () => {
    expect(parseHash(`#/market/${ID}`)).toEqual({ name: 'token', covenantId: ID });
    expect(parseHash(`#/market/${ID.toUpperCase()}`)).toEqual({ name: 'token', covenantId: ID });
    expect(parseHash(`#/market/${ID}/extra`)).toEqual({ name: 'token', covenantId: ID });
  });

  it('reports a malformed token id instead of guessing', () => {
    expect(parseHash('#/market/abc')).toEqual({ name: 'market', invalidToken: 'abc' });
    expect(parseHash(`#/market/${ID}zz`).name).toBe('market');
    expect(parseHash('#/market/%E0%A4%A')).toEqual({ name: 'market', invalidToken: '%E0%A4%A' }); // undecodable escape
  });

  it('ignores a query string', () => {
    expect(parseHash('#/orders?x=1')).toEqual({ name: 'orders' });
    expect(parseHash(`#/market/${ID}?side=buy`)).toEqual({ name: 'token', covenantId: ID });
  });

  it('a token link may open the ticket on "close" (close position, step 2); any other preset is ignored', () => {
    expect(parseHash(`#/market/${ID}?ticket=close`)).toEqual({ name: 'token', covenantId: ID, ticket: 'close' });
    expect(parseHash(`#/market/${ID}?ticket=evil`)).toEqual({ name: 'token', covenantId: ID });
    expect(routeToHash(tokenRoute(ID, 'close'))).toBe(`#/market/${ID}?ticket=close`);
    expect(parseHash(routeToHash(tokenRoute(ID, 'close')))).toEqual(tokenRoute(ID, 'close'));
  });

  it('a cover link (close of a sell-first position) carries the amount to buy back in base units; bad amounts are dropped', () => {
    expect(parseHash(`#/market/${ID}?ticket=cover&amount=7000`)).toEqual({ name: 'token', covenantId: ID, ticket: 'cover', amount: '7000' });
    expect(routeToHash(tokenRoute(ID, 'cover', 7000))).toBe(`#/market/${ID}?ticket=cover&amount=7000`);
    expect(parseHash(routeToHash(tokenRoute(ID, 'cover', 123_456_789_012n)))).toEqual(tokenRoute(ID, 'cover', 123_456_789_012n));
    expect(parseHash(`#/market/${ID}?ticket=cover&amount=0`)).toEqual({ name: 'token', covenantId: ID, ticket: 'cover' });
    expect(parseHash(`#/market/${ID}?ticket=cover&amount=-3`)).toEqual({ name: 'token', covenantId: ID, ticket: 'cover' });
    expect(parseHash(`#/market/${ID}?ticket=cover&amount=1.5`)).toEqual({ name: 'token', covenantId: ID, ticket: 'cover' });
    expect(parseHash(`#/market/${ID}?ticket=close&amount=3`)).toEqual({ name: 'token', covenantId: ID, ticket: 'close' });
    expect(tokenRoute(ID, 'cover', 0)).toEqual({ name: 'token', covenantId: ID, ticket: 'cover' });
  });

  it('round-trips every route', () => {
    const routes: Route[] = [{ name: 'market' }, tokenRoute(ID.toUpperCase()), { name: 'orders' }, { name: 'issue' }, { name: 'settings' }];
    for (const r of routes) expect(parseHash(routeToHash(r))).toEqual(r.name === 'token' ? tokenRoute(ID) : r);
    expect(routeToHash(tokenRoute(ID))).toBe(`#/market/${ID}`);
  });

  it('highlights the market nav on a token page', () => {
    expect(navOf(tokenRoute(ID))).toBe('market');
    expect(navOf({ name: 'orders' })).toBe('orders');
  });

  it('parses a pair route (two different 64-hex ids), round-trips it and highlights the market nav', () => {
    const Q = 'cd'.repeat(32);
    expect(parseHash(`#/pair/${ID.toUpperCase()}/${Q}`)).toEqual({ name: 'pair', base: ID, quote: Q });
    // the canonical form is the market URL; the old pair URL is an alias of it
    expect(routeToHash(pairRoute(ID, Q))).toBe(`#/market/${ID}/${Q}`);
    expect(parseHash(`#/market/${ID.toUpperCase()}/${Q}`)).toEqual({ name: 'pair', base: ID, quote: Q });
    expect(parseHash(`#/market/${ID}/kas`)).toEqual(tokenRoute(ID));
    expect(parseHash(`#/market/${ID}/${ID}`).name).toBe('market');
    expect(parseHash(`#/market/${ID}/zz`)).toEqual(tokenRoute(ID)); // not a quote: ignored, as any extra segment
    expect(parseHash(`#/market/${ID}/${'ab'.repeat(31)}`)).toEqual(tokenRoute(ID));
    expect(parseHash(routeToHash(pairRoute(ID, Q)))).toEqual(pairRoute(ID, Q));
    expect(navOf(pairRoute(ID, Q))).toBe('market');
    expect(parseHash(`#/pair/${ID}/${ID}`).name).toBe('market');
    expect(parseHash(`#/pair/${ID}`)).toMatchObject({ name: 'market' });
    expect(parseHash('#/pair/xyz/abc')).toMatchObject({ name: 'market', invalidToken: 'xyz/abc' });
  });
});
