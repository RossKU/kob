import { describe, expect, it } from 'vitest';
import { installable, KNOWN_WALLETS } from './wallet-model';

describe('the wallets offered for install', () => {
  it('every known wallet that was not detected, without Kastle unless it is switched on', () => {
    expect(installable([], false).map((w) => w.id)).toEqual(['kasware', 'kaspire']);
    expect(installable(['kasware'], false).map((w) => w.id)).toEqual(['kaspire']);
    expect(installable(['kasware'], true).map((w) => w.id)).toEqual(['kaspire', 'kastle']);
    expect(installable(['kasware', 'kaspire'], false)).toEqual([]);
  });

  it('every link is https', () => {
    for (const w of KNOWN_WALLETS) expect(w.installUrl.startsWith('https://'), w.id).toBe(true);
  });
});
