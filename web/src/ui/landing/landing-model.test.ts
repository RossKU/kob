import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { resolveConfig } from '../../config';
import { navOf, parseHash, routeToHash } from '../../app/router';
import { has, t } from '../../i18n';
import { APP_HASH, FEEDBACK_CONTACT_URL, feedbackActions, homeTargetOf, LANDING_NETWORKS, landingNetworkText, publicApiUrl, showsLanding, siteName } from './landing-model';

const A = 'a'.repeat(64);
const B = 'b'.repeat(64);
const tokens = [
  { covenantId: A, tradable: true },
  { covenantId: B, tradable: true },
];
const src = (p: string) => readFileSync(fileURLToPath(new URL(`../../${p}`, import.meta.url)), 'utf8');

describe('landing routing', () => {
  it('the empty hash shows the landing page by default (a visitor from outside), whatever the tokens or the USD reference', () => {
    const { config } = resolveConfig({});
    expect(config.home).toBe('landing');
    for (const h of ['', '#', '#/']) expect(showsLanding(parseHash(h), config, tokens)).toBe(true);
    expect(showsLanding(parseHash(''), { ...config, quoteTokens: { [B]: 'USD' } }, tokens)).toBe(true);
    expect(showsLanding(parseHash(''), config, [])).toBe(true);
  });

  it('every app route shows the app shell: the market list stays at #/market, deep links unchanged', () => {
    const { config } = resolveConfig({});
    expect(parseHash(APP_HASH)).toEqual({ name: 'market' });
    for (const h of ['#/market', `#/market/${A}`, `#/market/${A}/${B}`, `#/pair/${A}/${B}`, '#/orders', '#/create', '#/settings', '#/issue', '#/nope']) {
      expect(showsLanding(parseHash(h), config, tokens), h).toBe(false);
    }
    expect(parseHash(`#/market/${A}`)).toEqual({ name: 'token', covenantId: A });
    expect(parseHash(`#/market/${A}/${B}`)).toEqual({ name: 'pair', base: A, quote: B });
  });

  it("an operator's explicit config home is honoured inside the shell (no landing page)", () => {
    const at = (home: unknown, quoteTokens: Record<string, string> = {}) => {
      const { config } = resolveConfig({ file: { home, quoteTokens } });
      return { shows: showsLanding(parseHash('#/'), config, tokens), target: homeTargetOf(config, tokens) };
    };
    expect(at('auto')).toEqual({ shows: false, target: { name: 'token', covenantId: A } });
    expect(at('auto', { [B]: 'USD' })).toEqual({ shows: false, target: { name: 'token', covenantId: B } });
    expect(at('list')).toEqual({ shows: false, target: { name: 'market' } });
    expect(at(`market:${B}`)).toEqual({ shows: false, target: { name: 'token', covenantId: B } });
    expect(at(`usd:${A}`, { [B]: 'USD' })).toEqual({ shows: false, target: { name: 'pair', base: A, quote: B } });
    // an invalid value is reported and the default (the landing page) applies
    expect(at('chart-please').shows).toBe(true);
  });

  it('the header logo links to the empty hash (the landing page), the Market tab to #/market', () => {
    expect(routeToHash({ name: 'home' })).toBe('#/');
    const header = src('ui/shell/Header.tsx');
    expect(header).toContain(`<Brand href={routeToHash({ name: 'home' })} testid="brand" />`);
    expect(header).toContain(`{ name: 'market', href: '#/market'`);
    // an operator's chart at the empty hash is a market view: its tab is Market
    expect(navOf({ name: 'home' })).toBe('market');
  });

  it('the installed app (PWA) opens the market, not the landing page', () => {
    const m = JSON.parse(src('../public/manifest.webmanifest')) as { start_url: string; scope: string };
    expect(m.start_url).toBe('./#/market');
    expect(parseHash(new URL(m.start_url, 'https://kob.example/').hash)).toEqual({ name: 'market' });
    expect(m.scope).toBe('./');
  });
});

describe('landing network text', () => {
  it('names the configured network in the chip only; the rest of the page reads the same on every network', () => {
    const n = landingNetworkText('testnet-10', 'kob.kaspa.ag');
    expect(n.chip).toBe('testnet-10 · open review');
    expect(n.chipTip).toBe('Kaspa testnet-10. Open review: the specification is not final, and no review can show the code is free of bugs.');
    expect(n.auditBody).toBe('Verify it yourself: the code is public. The specification is not final, and no review can show the code is free of bugs. Red-team work and private reports are welcome.');
    expect(n.discLead).toBe('Open review.');
    expect(n.txNote).toBe('Illustrative example · not a real order');
    const m = landingNetworkText('mainnet', 'kob.kaspa.ag');
    expect([m.txNote, m.auditBody, m.discLead]).toEqual([n.txNote, n.auditBody, n.discLead]);
    expect(n.discBody).toContain('The specification is not final, and no review can show the code is free of bugs. kob.kaspa.ag publishes');
    expect(n.discBody).toContain('Test tokens have no value');
    expect(n.discBody).toContain('kob.kaspa.ag publishes software and information only');
    expect(n.discBody).toContain('Test tokens have no value');
  });

  it('never says testnet (or test tokens) on mainnet', () => {
    const n = landingNetworkText('mainnet', 'kob.kaspa.ag');
    for (const v of Object.values(n)) {
      expect(v).not.toMatch(/testnet|test token/i);
      expect(v).not.toMatch(/^[a-z]+\.[a-z.]+$/); // no untranslated key
    }
    expect(n.chip).toBe('mainnet · open review');
    expect(n.auditBody).toBe('Verify it yourself: the code is public. The specification is not final, and no review can show the code is free of bugs. Red-team work and private reports are welcome.');
    expect(n.discLead).toBe('Open review.');
    expect(n.discBody).toContain('Real funds are at risk');
  });

  it('no sentence of the page suggests an audit is coming, and each one says there was none', () => {
    for (const net of LANDING_NETWORKS) {
      const n = landingNetworkText(net, 'x');
      for (const v of Object.values(n)) expect(v).not.toMatch(/pre-audit|audit yet|not yet/i);
      for (const v of [n.chipTip, n.auditBody, n.discBody]) expect(v).toMatch(/no review can show the code is free of bugs/i);
    }
  });

  it('has every sentence for every network', () => {
    for (const net of LANDING_NETWORKS) for (const v of Object.values(landingNetworkText(net, 'x'))) expect(v).not.toMatch(/landing\./);
  });

  it('speaks for the host it is served from, or "This site" for a local one', () => {
    expect(siteName('kob.kaspa.ag')).toBe('kob.kaspa.ag');
    expect(siteName('KOB.Example.org')).toBe('kob.example.org');
    for (const h of ['', 'localhost', '127.0.0.1', '[::1]', null]) expect(siteName(h)).toBe('This site');
  });

  it('links the public API to the configured indexer, or hides it', () => {
    expect(publicApiUrl('https://kob-api.kaspa.ag')).toBe('https://kob-api.kaspa.ag/v1/health');
    expect(publicApiUrl('https://idx.example/base/')).toBe('https://idx.example/base/v1/health');
    expect(publicApiUrl('')).toBeNull();
    expect(publicApiUrl(undefined)).toBeNull();
    expect(publicApiUrl('javascript:alert(1)')).toBeNull();
  });
});

describe('the trust us row', () => {
  it('a security report, an issue and a message on X; no message action without a channel', () => {
    expect(FEEDBACK_CONTACT_URL).toBe('https://x.com/kaspaorderbook');
    expect(feedbackActions().map((a) => a.href)).toEqual(['https://github.com/RossKU/kob/security/advisories/new', 'https://github.com/RossKU/kob/issues', 'https://x.com/kaspaorderbook']);
    expect(feedbackActions('').map((a) => a.key)).toEqual(['landing.feedback.security', 'landing.feedback.issue']);
    for (const a of feedbackActions()) expect(has(a.key), a.key).toBe(true);
  });

  it('has the label, the value and no section of its own', () => {
    expect(t('landing.trust.audit')).toBe('Trust us');
    expect(has('landing.trust.audit.v')).toBe(false);
    expect(t('landing.trust.source')).toBe('Closed source');
    expect(t('landing.trust.source.body')).toContain('SilverScript');
    for (const k of ['label', 'l1', 'l2', 'intro']) expect(has(`landing.feedback.${k}`), k).toBe(false);
  });

  it('says there is no bug bounty, as SECURITY.md does', () => {
    expect(t('landing.trust.audit.note')).toBe('Please test only with your own funds and orders. There is no bug bounty.');
    expect(src('../../SECURITY.md')).toContain('There is no bug bounty.');
  });
});

describe('landing page source', () => {
  it('in-app links only for the app (a self-hosted copy stays on its host), no inline HTML', () => {
    const page = src('ui/landing/LandingPage.tsx');
    expect(page).not.toContain('kob.kaspa.ag');
    expect(page).not.toMatch(/dangerouslySetInnerHTML|innerHTML/);
    expect(page).not.toMatch(/href="#(?!\/)/); // a plain anchor would be a route here
  });
});
