// The header's wide row: the tabs in the true centre when the row has room for it, else packed after the brand; the brand's name where it still fits.
import { describe, expect, it } from 'vitest';
import { headerLayout, rowWidths } from './narrow';

const m = { logo: 32, name: 50, nav: 420, tools: 300, gap: 12 };

describe('header row layout', () => {
  it('packed = the parts side by side; centred = the tabs between two sides as wide as the wider of the brand and the tools', () => {
    expect(rowWidths(m, false)).toEqual({ packed: 32 + 420 + 300 + 24, centred: 420 + 600 + 24 });
    expect(rowWidths({ ...m, tools: 20 }, true)).toEqual({ packed: 82 + 420 + 20 + 24, centred: 420 + 164 + 24 });
  });

  it('centres the tabs only where the centred row fits (with the name where it is always shown), else packs them; the name follows the chosen row', () => {
    expect(headerLayout({ ...m, room: 1100 }, true)).toEqual({ centred: true, brandName: true });
    // room for the packed row with the name, not for the centred row
    expect(headerLayout({ ...m, room: 900 }, true)).toEqual({ centred: false, brandName: true });
    expect(headerLayout({ ...m, room: 830 }, false)).toEqual({ centred: false, brandName: true });
    expect(headerLayout({ ...m, room: 829 }, false)).toEqual({ centred: false, brandName: false });
    // a narrow tools side: the brand with its name is the wider side
    expect(headerLayout({ ...m, tools: 40, room: 700 }, false)).toEqual({ centred: true, brandName: true });
    expect(headerLayout({ ...m, tools: 40, room: 560 }, false)).toEqual({ centred: true, brandName: false });
  });
});
