// The gate on the contrast of the controls that change colour with their state: in both themes, every segmented control and every white pill, in each state it can be
// in, shows its text at 4.5:1 or more. The colours are the browser's own, read after the cascade: a hover rule that outranks the checked background (the `Rows` control:
// the text of a segment the pointer had just pressed took the colour of what it stood on) can only be seen this way. The maths: src/ui/kit/contrast.ts.
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { connectWallet, listExkcc } from '../helpers/env';
import { MIN_TEXT_CONTRAST, textContrast } from '../../src/ui/kit/contrast';

test.use({ autoOpen: false });

const THEMES = ['dark', 'light'] as const;
type Theme = (typeof THEMES)[number];

/** The segments of every kind: the kit's radio segments, and the Activity log's own (pressed instead of checked). */
const SEGMENTS = '.segmented button:not(:disabled), .act-seg button';
const CHECKED = "[aria-checked='true'], [aria-pressed='true']";

/** Colours read at once, with no transition under way. */
const still = (page: Page) => page.addStyleTag({ content: '*, *::before, *::after { transition: none !important; animation: none !important; }' });

async function open(page: Page, theme: Theme, route: string): Promise<void> {
  await page.addInitScript((th) => localStorage.setItem('kob.theme', th), theme);
  await page.goto(route);
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
  await still(page);
}

/** What the text of `el` stands on: its colour and the background colours from `el` up to the first opaque one. The selected segment is transparent: its pill is the sliding mark
 *  (`.slide-ind`, a sibling), which stands under it, so that mark's colour is what the text is on. */
const colours = (el: Locator) =>
  el.evaluate((node) => {
    const backgrounds: string[] = [];
    for (let e: Element | null = node; e; e = e.parentElement) {
      const bg = getComputedStyle(e).backgroundColor;
      backgrounds.push(bg);
      if (e === node && e.parentElement?.querySelector(':scope > .slide-ind') && bg === 'rgba(0, 0, 0, 0)' && e.matches("[aria-checked='true'], [aria-pressed='true']")) {
        backgrounds.push(getComputedStyle(e.parentElement.querySelector(':scope > .slide-ind')!).backgroundColor);
      }
      if (/^rgb\(/.test(bg)) break;
    }
    return { color: getComputedStyle(node).color, backgrounds };
  });

/** The pointer away and no focus: the state a control is in at rest. */
async function rest(page: Page): Promise<void> {
  await page.mouse.move(0, 0);
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
}

/** Collects the states below the limit, each named: one run lists them all. */
function checker(theme: Theme) {
  const low: string[] = [];
  return {
    low,
    async check(el: Locator, what: string, state: string): Promise<void> {
      const { color, backgrounds } = await colours(el);
      const ratio = textContrast(color, backgrounds);
      if (!(ratio >= MIN_TEXT_CONTRAST)) low.push(`${theme} ${what} (${state}): ${ratio.toFixed(2)}:1, ${color} on ${backgrounds.join(' over ')}`);
    },
  };
}

/** Every state of one control that is not a segment: at rest, under the pointer, with the focus. */
async function checkStates(page: Page, c: ReturnType<typeof checker>, el: Locator, what: string): Promise<void> {
  await rest(page);
  await c.check(el, what, 'default');
  await el.hover();
  await c.check(el, what, 'hover');
  await rest(page);
  await el.focus();
  await c.check(el, what, 'focus');
  await rest(page);
}

/** Every state of every segment on the page: pressed, a segment keeps the pointer, so checked and hover are read together as well as apart. */
async function checkSegments(page: Page, c: ReturnType<typeof checker>, where: string, expected: number): Promise<void> {
  const segments = page.locator(SEGMENTS);
  await expect.poll(() => segments.count(), { message: `the segments of ${where}` }).toBeGreaterThanOrEqual(expected);
  const n = await segments.count();
  for (let i = 0; i < n; i++) {
    const s = segments.nth(i);
    const what = `${where} segment "${(await s.innerText()).trim()}"`;
    await rest(page);
    if (!(await s.evaluate((e, sel) => e.matches(sel), CHECKED))) {
      await c.check(s, what, 'default');
      await s.hover();
      await c.check(s, what, 'hover');
      await rest(page);
      await s.focus();
      await c.check(s, what, 'focus');
      await rest(page);
      await s.click();
    } else {
      await s.hover();
    }
    await expect(s).toHaveAttribute((await s.getAttribute('role')) === 'radio' ? 'aria-checked' : 'aria-pressed', 'true');
    await c.check(s, what, 'checked, hover');
    await rest(page);
    await c.check(s, what, 'checked');
    await s.focus();
    await c.check(s, what, 'checked, focus');
  }
}

for (const theme of THEMES) {
  test.describe(`${theme} theme`, () => {
    test('the market list: Rows', async ({ appPage: page }) => {
      await open(page, theme, '/#/market');
      const c = checker(theme);
      await checkSegments(page, c, 'market list', 3);
      expect(c.low).toEqual([]);
    });

    test('the Activity page: its filters and Rows', async ({ appPage: page }) => {
      await open(page, theme, '/#/activity');
      const c = checker(theme);
      await checkSegments(page, c, 'activity', 6);
      expect(c.low).toEqual([]);
    });

    test('a token page: buy and sell, and the log', async ({ appPage: page, mock }) => {
      await listExkcc(page);
      const token = await mock.token();
      await open(page, theme, `/#/market/${token.covenant_id}`);
      await expect(page.getByTestId('order-side-buy')).toBeVisible();
      const c = checker(theme);
      await checkSegments(page, c, 'token page', 2);
      expect(c.low).toEqual([]);
    });

    test('Create token: the program', async ({ appPage: page }) => {
      await open(page, theme, '/#/create');
      await connectWallet(page);
      await page.getByTestId('issue-advanced').getByRole('button').first().click();
      await expect(page.getByTestId('issue-program')).toBeVisible();
      const c = checker(theme);
      await checkSegments(page, c, 'create token', 2);
      expect(c.low).toEqual([]);
    });

    test('a drop-down: its button, and the rows and headings of its list', async ({ appPage: page, mock }) => {
      await listExkcc(page);
      const token = await mock.token();
      await open(page, theme, `/#/market/${token.covenant_id}`);
      const face = page.getByTestId('order-type');
      await expect(face).toBeVisible();
      const c = checker(theme);
      await checkStates(page, c, face, 'drop-down button');
      await face.click();
      await c.check(face, 'drop-down button', 'open');
      const panel = page.getByTestId('order-type-panel');
      await expect(panel).toBeVisible();
      const label = (value: string) => page.getByTestId(`order-type-option-${value}`).locator('.picker-label');
      await c.check(label('limit'), 'row', 'chosen, active');
      await c.check(panel.locator('.picker-group').first(), 'heading', 'default');
      await c.check(label('market'), 'row', 'default');
      await page.getByTestId('order-type-option-market').hover();
      await c.check(label('market'), 'row', 'hover');
      await c.check(label('limit'), 'row', 'chosen');
      // no list of the app has a disabled row yet: the state is the attribute's
      await page.getByTestId('order-type-option-ioc').evaluate((e) => e.setAttribute('aria-disabled', 'true'));
      await c.check(label('ioc'), 'row', 'disabled');
      await page.keyboard.press('Escape');
      expect(c.low).toEqual([]);
    });

    test('the white pills: Connect, and Open the app in the header and the hero', async ({ appPage: page }) => {
      const c = checker(theme);
      await open(page, theme, '/#/market');
      const connect = page.locator('.header .btn-pill').first();
      await expect(connect).toBeVisible();
      await checkStates(page, c, connect, 'Connect');
      await page.goto('/');
      await expect(page.getByTestId('landing')).toBeVisible();
      await still(page);
      await checkStates(page, c, page.getByTestId('landing-open-app'), 'Open the app (header)');
      await checkStates(page, c, page.getByTestId('landing-hero').getByTestId('landing-cta'), 'Open the app (hero)');
      expect(c.low).toEqual([]);
    });
  });
}
