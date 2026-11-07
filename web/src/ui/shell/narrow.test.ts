// The header's wide row: the tabs in the true centre when the row has room for it, else packed after the brand; the brand's name where it still fits.
import { describe, expect, it } from 'vitest';
import { disconnectFits, headerLayout, rowWidths } from './narrow';

const m = { logo: 32, name: 50, nav: 420, tools: 300, gap: 12 };

describe('header row layout', () => {
  it('packed = the parts side by side; centred = the tabs between two sides as wide as the wider of the brand and the tools', () => {
    expect(rowWidths(m, false)).toEqual({ packed: 32 + 420 + 300 + 24, centred: 420 + 600 + 24 });
    expect(rowWidths({ ...m, tools: 20 }, true)).toEqual({ packed: 82 + 420 + 20 + 24, centred: 420 + 164 + 24 });
  });

  it('centres the tabs only where the centred row fits, else packs them; the name is shown where the chosen row still has room for it', () => {
    expect(headerLayout({ ...m, room: 1100 })).toEqual({ centred: true, brandName: true });
    // room for the packed row with the name, not for the centred row
    expect(headerLayout({ ...m, room: 900 })).toEqual({ centred: false, brandName: true });
    expect(headerLayout({ ...m, room: 830 })).toEqual({ centred: false, brandName: true });
    expect(headerLayout({ ...m, room: 829 })).toEqual({ centred: false, brandName: false });
    // a narrow tools side: the brand with its name is the wider side
    expect(headerLayout({ ...m, tools: 40, room: 700 })).toEqual({ centred: true, brandName: true });
    expect(headerLayout({ ...m, tools: 40, room: 560 })).toEqual({ centred: true, brandName: false });
  });
});

describe('the word beside the disconnect icon', () => {
  const m = { logo: 40, name: 60, nav: 400, tools: 300, gap: 16, room: 1000 };
  it('shows only where the row as arranged has room for it', () => {
    const packed = { centred: false, brandName: false };
    const used = rowWidths(m, false).packed;
    expect(disconnectFits({ ...m, room: used + 80 + 4 }, packed, 80)).toBe(true);
    expect(disconnectFits({ ...m, room: used + 80 + 3 }, packed, 80)).toBe(false);
  });
  it('needs a wallet: no word, nothing to show', () => {
    expect(disconnectFits(m, { centred: false, brandName: false }, 0)).toBe(false);
  });
});
