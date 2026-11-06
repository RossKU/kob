// public/_redirects (Cloudflare Pages): the old indexer relay path of kob.kaspa.ag goes to the public indexer, nothing else is redirected.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const web = (rel: string) => fileURLToPath(new URL(`../${rel}`, import.meta.url));
const rules = () =>
  readFileSync(web('public/_redirects'), 'utf8')
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('#'))
    .map((l) => l.split(/\s+/));

describe('public/_redirects', () => {
  it('sends /indexer and /indexer/* to https://kob-api.kaspa.ag with 308 (method and body kept), and only those', () => {
    expect(rules()).toEqual([
      ['/indexer', 'https://kob-api.kaspa.ag/', '308'],
      ['/indexer/*', 'https://kob-api.kaspa.ag/:splat', '308'],
    ]);
  });

  it('the indexer it names is the testnet-10 default of the deployment record', () => {
    const record = JSON.parse(readFileSync(web('../contracts/deploy/testnet-10/deployment.json'), 'utf8'));
    expect(record.indexer_url).toBe('https://kob-api.kaspa.ag');
  });
});
