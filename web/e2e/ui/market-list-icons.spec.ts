// The Market list's token icon is as tall as the row's text block allows, and never makes a row taller: the row is its text block plus its padding, whatever
// the icon. The fingerprint is 80 % of the icon and centred on it; the letter of a fallback icon scales with the icon.
import { test, expect } from '../fixtures';

test.use({ autoOpen: false });

for (const [width, iconMin, iconMax] of [[1280, 46, 48], [880, 40, 48], [390, 38, 42]] as const) {
  test(`${width} px: the icon fills the text block without growing the row`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 800 });
    await page.goto('/#/market');
    await expect(page.getByTestId('token-table')).toBeVisible();
    const rows = await page.evaluate(() =>
      [...document.querySelectorAll('[data-testid^="token-row-"]')].map((tr) => {
        const box = (sel: string) => tr.querySelector(sel)!.getBoundingClientRect();
        const icon = box('.tk-icon');
        const fp = box('.fp');
        const letter = getComputedStyle(tr.querySelector('.tk-icon')!).fontSize;
        return { row: tr.getBoundingClientRect().height, text: box('.ml-name-text').height, icon: icon.height, iconTop: icon.top, fp: fp.height, fpMid: fp.top + fp.height / 2, iconMid: icon.top + icon.height / 2, letter: parseFloat(letter) };
      }),
    );
    expect(rows.length).toBeGreaterThan(1);
    for (const r of rows) {
      // the row is its text block and its padding (7 px on a desktop, a card's on a phone): the icon is not what sets the height
      expect(r.icon).toBeLessThanOrEqual(r.text + 0.5);
      expect(r.row - r.text).toBeGreaterThan(5);
      expect(r.row - r.text).toBeLessThan(width < 600 ? 14 : 8);
      expect(r.icon).toBeGreaterThanOrEqual(iconMin);
      expect(r.icon).toBeLessThanOrEqual(iconMax);
      // the fingerprint: 80 % of the icon, centred on it
      expect(Math.abs(r.fp - r.icon * 0.8)).toBeLessThan(1.5);
      expect(Math.abs(r.fpMid - r.iconMid)).toBeLessThan(1.5);
      // the letter follows the icon
      expect(Math.abs(r.letter - Math.max(12, r.icon * 0.45))).toBeLessThan(1);
    }
  });
}
