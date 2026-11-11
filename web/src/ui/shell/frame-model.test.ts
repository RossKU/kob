import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { footerLinks, showsChartNotice, soonFooterLinks } from './frame-model';

const src = (p: string) => readFileSync(fileURLToPath(new URL(`../../${p}`, import.meta.url)), 'utf8');

describe('the shared footer', () => {
  it('the app footer carries the chart library attribution; the landing page (no charts) does not', () => {
    expect(showsChartNotice('app')).toBe(true);
    expect(showsChartNotice('landing')).toBe(false);
    // the attribution itself: TradingView's NOTICE text and a link to tradingview.com, under its test id
    const frame = src('ui/shell/frame-parts.tsx');
    expect(frame).toContain('data-testid="footer-chart-notice"');
    expect(frame).toContain('href="https://www.tradingview.com/"');
    expect(frame).toContain('TradingView Lightweight Charts™');
    expect(frame).toContain('© 2025 TradingView, Inc.');
  });

  it('the landing page has the full row: App, GitHub, Run an executor, then the Public API only when an indexer is configured, then the Disclaimer', () => {
    const keys = (url: string | null) => footerLinks('landing', url).map((l) => l.key);
    expect(keys(null)).toEqual(['landing.footer.app', 'landing.github', 'landing.footer.executor', 'landing.footer.disclaimer']);
    expect(keys('not a url')).not.toContain('landing.footer.api');
    const api = footerLinks('landing', 'https://idx.example/').find((l) => l.key === 'landing.footer.api');
    expect(api).toMatchObject({ href: 'https://idx.example/v1/health', testid: 'landing-api' });
  });

  it('an app page has ONE compact line: GitHub and the Disclaimer only (the security policy is the footer itself); the rest is on the landing page', () => {
    const app = footerLinks('app', 'https://idx.example');
    expect(app.map((l) => l.key)).toEqual(['landing.github', 'landing.footer.disclaimer']);
    expect(app.some((l) => l.group === 'app')).toBe(false);
    expect(app.at(-1)).toMatchObject({ href: '#/disclaimer', testid: 'footer-disclaimer' });
  });

  it('Support stands before the Disclaimer on every page, only when there is a donation address', () => {
    for (const ctx of ['landing', 'app'] as const) {
      const site = footerLinks(ctx, null, true).filter((l) => l.group === 'site');
      expect(site.slice(-2).map((l) => l.key)).toEqual(['landing.footer.support', 'landing.footer.disclaimer']);
      expect(site.at(-2)).toMatchObject({ href: '#/support', testid: 'footer-support' });
      expect(footerLinks(ctx, null).some((l) => l.key === 'landing.footer.support')).toBe(false);
    }
  });

  it('every link text exists in the dictionary', () => {
    for (const l of [...footerLinks('app', 'https://idx.example', true), ...footerLinks('landing', 'https://idx.example', true)]) {
      expect(has(l.key), l.key).toBe(true);
      if (l.title) expect(has(l.title), l.title).toBe(true);
    }
  });

  it('the landing-only footer has the testnet app, GitHub and Run an executor, and no link into an app that is not there', () => {
    const links = soonFooterLinks();
    expect(links.map((l) => [l.key, l.href])).toEqual([
      ['landing.soon.try', 'https://kob-tn10.kaspa.ag/#/market'],
      ['landing.github', 'https://github.com/RossKU/kob'],
      ['landing.footer.executor', 'https://github.com/RossKU/kob'],
    ]);
    for (const l of links) {
      expect(has(l.key), l.key).toBe(true);
      expect(l.group).toBe('site');
      expect(l.href.startsWith('#'), l.href).toBe(false);
    }
  });
});
