import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { CHART_ATTRIBUTION_SVG, allowHtml, installTrustedTypesPolicy } from './trusted-types';

describe('Trusted Types default policy', () => {
  it('lets only the chart attribution logo through', () => {
    expect(allowHtml(CHART_ATTRIBUTION_SVG)).toBe(CHART_ATTRIBUTION_SVG);
    for (const s of ['', '<img src=x onerror=alert(1)>', CHART_ATTRIBUTION_SVG + '<script>alert(1)</script>', CHART_ATTRIBUTION_SVG.replace('35', '36')]) {
      expect(allowHtml(s), s).toBeNull();
    }
  });

  it('the constant is the markup the installed lightweight-charts writes', () => {
    const dist = fileURLToPath(new URL('../../node_modules/lightweight-charts/dist/lightweight-charts.production.mjs', import.meta.url));
    expect(readFileSync(dist, 'utf8')).toContain(`innerHTML='${CHART_ATTRIBUTION_SVG}'`);
  });

  it('installs one `default` policy with only createHTML; tolerates browsers without Trusted Types', () => {
    const made: [string, Record<string, unknown>][] = [];
    expect(installTrustedTypesPolicy({ trustedTypes: { createPolicy: (n, r) => made.push([n, r]) } })).toBe(true);
    expect(made.map(([n, r]) => [n, Object.keys(r)])).toEqual([['default', ['createHTML']]]);
    expect(installTrustedTypesPolicy({ trustedTypes: { createPolicy: () => { throw new Error('exists'); } } })).toBe(false);
    expect(installTrustedTypesPolicy({})).toBe(false);
  });
});
