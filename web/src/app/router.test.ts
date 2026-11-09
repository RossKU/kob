import { describe, expect, it } from 'vitest';
import { activityRoute, navOf, parseHash, PORTFOLIO_TABS, routeToHash, tokenRoute, type Route, pairRoute } from './router';

const ID = 'ab'.repeat(32);

describe('router', () => {
  it('an empty hash is the landing screen, an unknown one the market list', () => {
    for (const h of ['', '#', '#/', '#///']) expect(parseHash(h)).toEqual({ name: 'home' });
    for (const h of ['#/nope', '#/market', '#/market/', '#/unknown/page']) expect(parseHash(h)).toEqual({ name: 'market' });
    expect(routeToHash({ name: 'home' })).toBe('#/');
    expect(navOf({ name: 'home' })).toBe('market');
  });

  it('the old addresses are gone: #/usd, #/pair and #/issue are unknown pages (the market list)', () => {
    for (const h of ['#/usd/kas', `#/usd/${ID}`, `#/pair/${ID}/${'cd'.repeat(32)}`, '#/issue', '#/orders']) expect(parseHash(h)).toEqual({ name: 'market' });
  });

  it('parses the pages', () => {
    expect(parseHash('#/portfolio')).toEqual({ name: 'portfolio' });
    expect(parseHash('#/orders')).toEqual({ name: 'market' }); // the old address is gone
    expect(parseHash('#/create')).toEqual({ name: 'create' });
    expect(parseHash('#/settings')).toEqual({ name: 'settings' });
    expect(parseHash('/portfolio')).toEqual({ name: 'portfolio' });
    expect(parseHash('#/PORTFOLIO/')).toEqual({ name: 'portfolio' });
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
    expect(parseHash('#/portfolio?x=1')).toEqual({ name: 'portfolio' });
    // the Portfolio page's tabs: one of the four, any other segment is the page itself
    expect(parseHash('#/portfolio/open')).toEqual({ name: 'portfolio', tab: 'open' });
    expect(parseHash('#/portfolio/Fills')).toEqual({ name: 'portfolio', tab: 'fills' });
    expect(parseHash('#/portfolio/nope')).toEqual({ name: 'portfolio' });
    expect(parseHash('#/portfolio/recover')).toEqual({ name: 'portfolio', recover: true });
    expect(routeToHash({ name: 'portfolio', recover: true })).toBe('#/portfolio/recover');
    for (const tab of PORTFOLIO_TABS) expect(parseHash(routeToHash({ name: 'portfolio', tab }))).toEqual({ name: 'portfolio', tab });
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

  it('a sell link (close of a buy-first position) carries the amount the position bought, like cover', () => {
    expect(parseHash(`#/market/${ID}?ticket=sell&amount=250000000`)).toEqual({ name: 'token', covenantId: ID, ticket: 'sell', amount: '250000000' });
    expect(routeToHash(tokenRoute(ID, 'sell', 250_000_000n))).toBe(`#/market/${ID}?ticket=sell&amount=250000000`);
    expect(parseHash(routeToHash(tokenRoute(ID, 'sell', 10n ** 18n - 1n)))).toEqual(tokenRoute(ID, 'sell', 10n ** 18n - 1n));
    expect(parseHash(`#/market/${ID}?ticket=sell&amount=0`)).toEqual({ name: 'token', covenantId: ID, ticket: 'sell' });
    expect(parseHash(`#/market/${ID}?ticket=sell&amount=1e9`)).toEqual({ name: 'token', covenantId: ID, ticket: 'sell' });
  });

  it('round-trips every route', () => {
    const routes: Route[] = [{ name: 'market' }, tokenRoute(ID.toUpperCase()), { name: 'portfolio' }, { name: 'create' }, { name: 'settings' }];
    for (const r of routes) expect(parseHash(routeToHash(r))).toEqual(r.name === 'token' ? tokenRoute(ID) : r);
    expect(routeToHash(tokenRoute(ID))).toBe(`#/market/${ID}`);
  });

  it('highlights the market nav on a token page', () => {
    expect(navOf(tokenRoute(ID))).toBe('market');
    expect(navOf({ name: 'portfolio' })).toBe('portfolio');
  });

  it('parses a pair route (two different 64-hex ids), round-trips it and highlights the market nav', () => {
    const Q = 'cd'.repeat(32);
    expect(routeToHash(pairRoute(ID, Q))).toBe(`#/market/${ID}/${Q}`);
    expect(parseHash(`#/market/${ID.toUpperCase()}/${Q}`)).toEqual({ name: 'pair', base: ID, quote: Q });
    expect(parseHash(`#/market/${ID}/kas`)).toEqual(tokenRoute(ID));
    expect(parseHash(`#/market/${ID}/${ID}`).name).toBe('market');
    expect(parseHash(`#/market/${ID}/zz`)).toEqual(tokenRoute(ID)); // not a quote: ignored, as any extra segment
    expect(parseHash(`#/market/${ID}/${'ab'.repeat(31)}`)).toEqual(tokenRoute(ID));
    expect(parseHash(routeToHash(pairRoute(ID, Q)))).toEqual(pairRoute(ID, Q));
    expect(navOf(pairRoute(ID, Q))).toBe('market');
    expect(parseHash(`#/market/xyz/${Q}`)).toMatchObject({ name: 'market', invalidToken: `xyz/${Q}` });
  });

  it('the activity page: every market, or the one its link names (a KAS book of one token, or a pair); anything else is every market', () => {
    const Q = 'cd'.repeat(32);
    expect(parseHash('#/activity')).toEqual({ name: 'activity' });
    expect(navOf({ name: 'activity' })).toBe('activity');
    expect(parseHash(`#/activity?market=${ID.toUpperCase()}`)).toEqual({ name: 'activity', market: { kind: 'token', token: ID } });
    expect(parseHash(`#/activity?market=${ID}/${Q}`)).toEqual({ name: 'activity', market: { kind: 'pair', base: ID, quote: Q } });
    expect(parseHash(`#/activity?market=${ID}%2F${Q}`)).toEqual({ name: 'activity', market: { kind: 'pair', base: ID, quote: Q } });
    for (const bad of ['nope', `${ID}/${ID}`, `${ID}/x`, `${ID}/${Q}/${Q}`, '']) expect(parseHash(`#/activity?market=${bad}`)).toEqual({ name: 'activity' });
    for (const r of [activityRoute(), activityRoute({ kind: 'token', token: ID }), activityRoute({ kind: 'pair', base: ID, quote: Q })]) expect(parseHash(routeToHash(r))).toEqual(r);
    expect(routeToHash(activityRoute({ kind: 'pair', base: ID, quote: Q }))).toBe(`#/activity?market=${ID}/${Q}`);
  });
});
