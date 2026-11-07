import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { decodeAddress } from '../../../mock/address.mjs';
import { parseHash, routeToHash } from '../../app/router';
import { contributionsUrl, rankContributors } from '../../data/contributors';
import { routeWithSupport, SUPPORT_ADDRESS, SUPPORT_TITLES, supportAddressOf, titleOfRank } from './support-model';

// made-up, of the right shape: not a real address
const TEST_ADDRESS = `kaspatest:${'q'.repeat(61)}`;
const LIVE = { test: false };
const TEST = { test: true };

afterEach(() => vi.unstubAllGlobals());

describe('the titles go by rank', () => {
  it('1, 2, 4, 8, 16 and 32 places, in this order', () => {
    expect(SUPPORT_TITLES.map((t) => [t.name, t.places])).toEqual([
      ['Archguardian', 1],
      ['Guardian', 2],
      ['Warden', 4],
      ['Sentinel', 8],
      ['Watcher', 16],
      ['Lookout', 32],
    ]);
  });

  it('every boundary, on both sides', () => {
    const at: [number, string | null][] = [
      [1, 'Archguardian'],
      [2, 'Guardian'],
      [3, 'Guardian'],
      [4, 'Warden'],
      [7, 'Warden'],
      [8, 'Sentinel'],
      [15, 'Sentinel'],
      [16, 'Watcher'],
      [31, 'Watcher'],
      [32, 'Lookout'],
      [63, 'Lookout'],
      [64, null],
      [65, null],
      [5000, null],
    ];
    for (const [rank, title] of at) expect(titleOfRank(rank), `rank ${rank}`).toBe(title);
  });

  it('a tie in the total goes to the earlier first contribution, and the title follows', () => {
    const payment = (sender: string, time: number) => ({ txid: String(time), sender, amount: 100n, time });
    const late = `kaspa:${'p'.repeat(61)}`;
    const early = `kaspa:${'z'.repeat(61)}`;
    const ranked = rankContributors([payment(late, 2_000), payment(early, 1_000)]);
    expect(ranked.map((c, i) => [c.address, titleOfRank(i + 1)])).toEqual([
      [early, 'Archguardian'],
      [late, 'Guardian'],
    ]);
  });
});

describe('the address switches the page on', () => {
  it('the page is on, with the constant as it stands', () => {
    expect(supportAddressOf(LIVE)).toBe(SUPPORT_ADDRESS);
    expect(SUPPORT_ADDRESS).not.toBe('');
  });

  it('the address is a mainnet one with a good checksum, read from the mainnet explorer whatever the site runs on, and in the README too (the page says it is)', () => {
    expect(SUPPORT_ADDRESS.startsWith('kaspa:')).toBe(true);
    expect(contributionsUrl(SUPPORT_ADDRESS, null).startsWith(`https://api.kaspa.org/addresses/${SUPPORT_ADDRESS}/`)).toBe(true);
    expect(decodeAddress(SUPPORT_ADDRESS)).not.toBeNull();
    expect(readFileSync(fileURLToPath(new URL('../../../../README.md', import.meta.url)), 'utf8')).toContain(SUPPORT_ADDRESS);
  });

  it('empty or not an address: none', () => {
    expect(supportAddressOf(LIVE, '')).toBe('');
    expect(supportAddressOf(LIVE, 'kaspa:typo')).toBe('');
    expect(supportAddressOf(LIVE, TEST_ADDRESS)).toBe(TEST_ADDRESS);
  });

  it('a test address is taken only with the test feature on', () => {
    vi.stubGlobal('window', { __KOB_TEST_SUPPORT_ADDRESS__: TEST_ADDRESS });
    expect(supportAddressOf(TEST, '')).toBe(TEST_ADDRESS);
    expect(supportAddressOf(LIVE, '')).toBe('');
  });

  it('#/support is the market list without an address, its page with one', () => {
    const route = parseHash('#/support');
    expect(route).toEqual({ name: 'support' });
    expect(routeToHash(route)).toBe('#/support');
    expect(routeWithSupport(route, '')).toEqual({ name: 'market' });
    expect(routeWithSupport(route, TEST_ADDRESS)).toBe(route);
    const other = parseHash('#/settings');
    expect(routeWithSupport(other, '')).toBe(other);
  });
});
