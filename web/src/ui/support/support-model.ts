// The Support page's facts (pure, tested in support-model.test.ts): the donation address, the contributors' titles, whether the page exists at all.
import type { Route } from '../../app/router';
import { isKaspaAddress } from '../../data/contributors';

declare global {
  interface Window {
    /** e2e only, read with `features.test` on: the address the specs run the page with */
    __KOB_TEST_SUPPORT_ADDRESS__?: string;
  }
}

/**
 * The project's donation address, a Kaspa mainnet one: the page shows it and reads its contributors from the mainnet explorer on every deployment, a testnet site
 * included. It is published in the root README.md too, as the page says: change both together. Empty would mean no Support link and `#/support` as the market list.
 */
export const SUPPORT_ADDRESS: string = 'kaspa:qq85cdj959gx33su28luhaymtq3yczx0jj40eufxwyxs77mk4vnpqd3m0nqqc';

/** The contributors' titles, in order of rank: the first `places` people get the first title, the next `places` the second, and so on. Below the table: no title. */
export const SUPPORT_TITLES: readonly { name: string; places: number; emoji: string }[] = [
  { name: 'Archguardian', places: 1, emoji: '👑' },
  { name: 'Guardian', places: 2, emoji: '🛡️' },
  { name: 'Warden', places: 4, emoji: '🗝️' },
  { name: 'Sentinel', places: 8, emoji: '⚔️' },
  { name: 'Watcher', places: 16, emoji: '🔭' },
  { name: 'Lookout', places: 32, emoji: '🏮' },
];

/** The title (name and emoji) of rank `rank` (1 = first), null past the end of the table. */
export function titleEntryOfRank(rank: number): { name: string; emoji: string } | null {
  let last = 0;
  for (const t of SUPPORT_TITLES) {
    last += t.places;
    if (rank <= last) return t;
  }
  return null;
}

/** The title name of rank `rank` (1 = first), null past the end of the table. */
export function titleOfRank(rank: number): string | null {
  return titleEntryOfRank(rank)?.name ?? null;
}

/** The row tint of the top ranks: gold for the first, silver for 2-3, bronze for 4-7 (styles.css `--tier-*`); none below. */
export function tierOfRank(rank: number): 'gold' | 'silver' | 'bronze' | null {
  return rank === 1 ? 'gold' : rank <= 3 ? 'silver' : rank <= 7 ? 'bronze' : null;
}

/** The address the page runs with: `SUPPORT_ADDRESS`, under test the injected one; '' when there is none or it is not an address. */
export function supportAddressOf(features: { test: boolean }, constant: string = SUPPORT_ADDRESS): string {
  const injected = features.test && typeof window !== 'undefined' ? window.__KOB_TEST_SUPPORT_ADDRESS__ : undefined;
  const address = injected ?? constant;
  return isKaspaAddress(address) ? address : '';
}

/** Without an address there is no Support page: its route is the market list, as an unknown hash is. */
export const routeWithSupport = (route: Route, address: string): Route => (route.name === 'support' && !address ? { name: 'market' } : route);
