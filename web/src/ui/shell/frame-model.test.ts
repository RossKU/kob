import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { footerLinks, showsChartNotice } from './frame-model';

const src = (p: string) => readFileSync(fileURLToPath(new URL(`../../${p}`, import.meta.url)), 'utf8');

describe('the shared footer', () => {
  it('the app footer carries the chart library attribution; the landing page (no charts) does not', () => {
    expect(showsChartNotice('app')).toBe(true);
    expect(showsChartNotice('landing')).toBe(false);
    // the attribution itself: TradingView's NOTICE text and a link to tradingview.com, under its test id
    const frame = src('ui/shell/Frame.tsx');
    expect(frame).toContain('data-testid="footer-chart-notice"');
    expect(frame).toContain('href="https://www.tradingview.com/"');
    expect(frame).toContain('TradingView Lightweight Charts™');
    expect(frame).toContain('© 2025 TradingView, Inc.');
  });

  it('one row of links: App, GitHub, Run an executor, then the Public API only when an indexer is configured', () => {
    const keys = (ctx: 'app' | 'landing', url: string | null) => footerLinks(ctx, url).map((l) => l.key);
    expect(keys('landing', null)).toEqual(['landing.footer.app', 'landing.github', 'landing.footer.executor']);
    expect(keys('landing', 'not a url')).not.toContain('landing.footer.api');
    const api = footerLinks('landing', 'https://idx.example/').find((l) => l.key === 'landing.footer.api');
    expect(api).toMatchObject({ href: 'https://idx.example/v1/health', testid: 'landing-api' });
    expect(footerLinks('app', 'https://idx.example').find((l) => l.key === 'landing.footer.api')?.testid).toBe('footer-api');
  });

  it("the app adds its own links after the site's: Settings and data, Recover orders, kaspa.org (a new tab)", () => {
    const app = footerLinks('app', null);
    expect(app.filter((l) => l.group === 'site').map((l) => l.key)).toEqual(['landing.footer.app', 'landing.github', 'landing.footer.executor']);
    expect(app.filter((l) => l.group === 'app').map((l) => [l.key, l.href, !!l.external])).toEqual([
      ['shell.footer.settings', '#/settings', false],
      ['shell.footer.recover', '#/orders', false],
      ['shell.footer.kaspa', 'https://kaspa.org', true],
    ]);
    expect(footerLinks('landing', 'https://idx.example').some((l) => l.group === 'app')).toBe(false);
    // the App link stays in the app's own origin (a self-hosted copy stays on its host)
    expect(app[0]!.href).toBe('#/market');
  });

  it('every link text exists in the dictionary', () => {
    for (const l of footerLinks('app', 'https://idx.example')) expect(has(l.key), l.key).toBe(true);
  });
});
