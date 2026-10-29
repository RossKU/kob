// The Market list's token icon is as tall as the row's text block allows, and never makes a row taller: the row is its text block plus its padding, whatever
// the icon. The fingerprint is 60 % of the icon and centred on it; the letter of a fallback icon scales with the icon. Every row has the same three lines (the
// ticker and the name on one line, the id fragment, the badges on their own line), so rows with no fact badge, one and two, and a long name are all as tall: one icon pitch.
import { readFileSync } from 'node:fs';
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
      // the fingerprint: 60 % of the icon, centred on it
      expect(Math.abs(r.fp - r.icon * 0.6)).toBeLessThan(1.5);
      expect(Math.abs(r.fpMid - r.iconMid)).toBeLessThan(1.5);
      // the letter follows the icon
      expect(Math.abs(r.letter - Math.max(12, r.icon * 0.45))).toBeLessThan(1);
    }
  });
}

const GENESIS = { txid: 'ab'.repeat(32), daa_score: 10, outputs: [1, 2], supply: 1000, minter_outputs: [], checked_at_daa: 20, source: 'test' };
const LONG = 'Kasanova KOB Cancellation Acceptance (declared by its creator) W'; // 64 characters: the longest name a registry entry may have

/** The example registry with three more listed tokens that differ in their badges (the family badge alone; one fact beside it; two) and carry a long name. */
async function listWithBadges(page: import('@playwright/test').Page): Promise<string[]> {
  const reg = JSON.parse(readFileSync(new URL('../../../registry/tokens.example.json', import.meta.url), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  const base = reg.tokens.find((t: { ticker: string }) => t.ticker === 'EXKCC');
  const mk = (n: number, ticker: string, extra: Record<string, unknown>) => ({ ...base, ticker, name: LONG, covenant_id: n.toString(16).repeat(64).slice(0, 64), verified: true, ...extra });
  const none = mk(0xa, 'ZERO', { genesis_verified: true, genesis: { ...GENESIS, live_minters: [] } });
  const one = mk(0xb, 'ONE', {});
  const two = mk(0xc, 'TWO', { genesis_verified: false, genesis: { ...GENESIS, live_minters: ['cd'.repeat(32) + ':3'] } });
  reg.tokens.push(none, one, two);
  await page.route('**/registry/tokens.json', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(reg) }),
  );
  return [none.covenant_id, one.covenant_id, two.covenant_id];
}

for (const width of [1280, 880, 390]) {
  test(`${width} px: rows with no fact badge, one and two, and a long name are all the same height; the name is cut on one line with its full text in the title`, async ({ appPage: page }) => {
    await page.setViewportSize({ width, height: 900 });
    const ids = await listWithBadges(page);
    await page.goto('/#/market');
    for (const id of ids) await expect(page.getByTestId(`token-row-${id}`)).toBeVisible();
    const rows = await page.evaluate((want) => {
      return want.map((id) => {
        const tr = document.querySelector(`[data-testid="token-row-${id}"]`)!;
        const rect = (sel: string) => tr.querySelector(sel)!.getBoundingClientRect();
        const sub = tr.querySelector('.ml-sub') as HTMLElement;
        const link = tr.querySelector('.token-name') as HTMLElement;
        return {
          badges: tr.querySelectorAll('.ml-line3 .badge').length,
          row: tr.getBoundingClientRect().height,
          rowTop: tr.getBoundingClientRect().top,
          icon: rect('.tk-icon').height,
          iconTop: rect('.tk-icon').top,
          line1: rect('.ml-line1').height,
          line2: rect('.ml-line2').height,
          line3: rect('.ml-line3').height,
          sub: { title: sub.getAttribute('title'), cut: sub.scrollWidth > sub.clientWidth, oneLine: sub.getBoundingClientRect().height < 30 },
          link: { title: link.getAttribute('title'), oneLine: link.getBoundingClientRect().height < 30 },
          idWidth: rect('.ml-line2 .ml-id').width,
          textWidth: rect('.ml-name-text').width,
        };
      });
    }, ids);
    // the three rows really differ in their badges: the family badge alone, and one and two facts beside it
    expect(rows.map((r) => r.badges)).toEqual([1, 2, 3]);
    for (const r of rows) {
      expect(r.row).toBeCloseTo(rows[0]!.row, 0);
      expect(r.line1).toBeCloseTo(rows[0]!.line1, 0);
      expect(r.line2).toBeCloseTo(rows[0]!.line2, 0);
      expect(r.line3).toBeCloseTo(rows[0]!.line3, 0);
      expect(r.icon).toBeCloseTo(rows[0]!.icon, 0);
      // the long name: one line, cut, the whole of it in the title
      expect(r.sub.title).toBe(LONG);
      if (width <= 400) expect(r.sub.cut).toBe(true);
      expect(r.sub.oneLine).toBe(true);
      expect(r.link.oneLine).toBe(true);
      expect(r.idWidth).toBeLessThanOrEqual(r.textWidth + 0.5);
    }
    // one icon pitch: every row of the list (these three and the others) is the same height, so the icons are the same distance apart
    const tops = await page.evaluate(() => [...document.querySelectorAll('[data-testid^="token-row-"] .tk-icon')].map((el) => el.getBoundingClientRect().top));
    expect(tops.length).toBeGreaterThan(3);
    const pitches = new Set(tops.slice(1).map((t, i) => Math.round(t - tops[i]!)));
    expect([...pitches]).toHaveLength(1);
  });
}
