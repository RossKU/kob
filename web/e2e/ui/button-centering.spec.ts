// The label of a button, a pill or a chip sits on the vertical centre of its box. A font whose ascent is much larger than its descent (Segoe UI and Yu Gothic UI,
// the system UI fonts of Windows) puts the cap height below the centre of a line box that flexbox centres; `text-box: trim-both cap alphabetic` (styles.css,
// landing.css) centres the cap height instead. The check: the box of the capital letters (from the baseline to the cap height) of the label's text node,
// against the centre of the control, within 1 px; an icon or the spinner of a busy button against the centre too; and the control's height as written.
// Chromium 133+ only (the property); a browser without it is skipped. KOB_BTN_SHOTS=<dir> also saves a zoomed screenshot of every control (device scale 3)
// and a JSONL of the measurements, named by KOB_BTN_TAG (before / after); KOB_WIDE_FONTS=1 runs the whole page in a wide font (the CI runner's).
import { appendFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../fixtures';
import { mockKascov, programHash, type MockKascovToken } from '../helpers/kascov';

test.use({ autoOpen: false, deviceScaleFactor: 3 });

const ID = (n: number) => n.toString(16).padStart(4, '0').repeat(16);
const tokens: MockKascovToken[] = Array.from({ length: 60 }, (_, i) => ({ covenant_id: ID(0x1000 + i), claimed_ticker: `T${String(i).padStart(3, '0')}`, claimed_name: `Token ${i}`, program: programHash('KCC20PublicMint') }));
const shots = process.env.KOB_BTN_SHOTS;
const tag = process.env.KOB_BTN_TAG ?? 'run';
const wide = process.env.KOB_WIDE_FONTS === '1';
/** KOB_BTN_FONT="Segoe UI": every element in that family (the system UI font of most Windows machines; the machine's own `system-ui` may be another). */
const forceFont = async (page: Page): Promise<void> => {
  const family = process.env.KOB_BTN_FONT;
  if (!family) return;
  await page.addInitScript((f) => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = `*, *::before, *::after { font-family: "${f}", sans-serif !important; }`;
      document.head.append(st);
    });
  }, family);
};
const tagged = `${tag}${wide ? '-wide' : ''}${process.env.KOB_BTN_FONT ? `-${process.env.KOB_BTN_FONT.replace(/[^A-Za-z0-9]+/g, '')}` : ''}`;
const record = (name: string, m: Measure) => {
  if (!shots) return;
  mkdirSync(shots, { recursive: true });
  appendFileSync(join(shots, `${tagged}-measure.jsonl`), `${JSON.stringify({ name, ...m })}\n`);
};

interface Measure {
  /** the first non-blank text node's cap box centre minus the control's centre, px (null: no text) */
  cap: number | null;
  /** the same for the ink of the whole string (descenders and all) */
  ink: number | null;
  /** an svg / spinner inside: its centre minus the control's centre */
  icon: number | null;
  height: number;
  font: string;
}

/** The vertical centre of the ink of the control's face, minus the control's centre (CSS px), with its background, border and colours forced to black on white, an icon hidden; `text`: the label's text node is set to it meanwhile. */
async function inkRows(page: Page, el: Locator, text: string | null): Promise<number | null> {
  const prepared = await el.evaluate((node, t) => {
    const w = document.createTreeWalker(node, NodeFilter.SHOW_TEXT, { acceptNode: (n) => (n.nodeValue && n.nodeValue.trim() ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT) });
    const nodes: Node[] = [];
    for (let n = w.nextNode(); n; n = w.nextNode()) nodes.push(n);
    if (!nodes.length) return false;
    (node as HTMLElement).dataset.probe = '1';
    if (!document.getElementById('probe-style')) {
      const s = document.createElement('style');
      s.id = 'probe-style';
      s.textContent =
        '[data-probe], [data-probe] * { color: #000 !important; background: #fff !important; border-color: #fff !important; box-shadow: none !important; text-shadow: none !important; outline: 0 !important; opacity: 1 !important; transform: none !important; transition: none !important; animation: none !important; visibility: visible !important; }' +
        '[data-probe] svg, [data-probe] .spinner, [data-probe]::before, [data-probe]::after { visibility: hidden !important; }';
      document.head.append(s);
    }
    if (t !== null) {
      (node as HTMLElement & { _saved?: [Node, string][] })._saved = nodes.map((n) => [n, n.nodeValue!]);
      nodes.forEach((n, i) => (n.nodeValue = i === 0 ? t : ''));
    }
    return true;
  }, text);
  if (!prepared) return null;
  // a whole-pixel window around the control (its own box sits at a fraction of a pixel on the page), scanned inside the control's box only
  const box = await el.evaluate((node) => {
    const r = node.getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  });
  const clip = { x: Math.floor(box.x), y: Math.floor(box.y) - 1, width: Math.ceil(box.w) + 2, height: Math.ceil(box.h) + 3 };
  const png = await page.screenshot({ clip, animations: 'disabled' });
  await el.evaluate((node) => {
    const n = node as HTMLElement & { _saved?: [Node, string][] };
    for (const [tn, v] of n._saved ?? []) tn.nodeValue = v;
    delete n._saved;
    delete n.dataset.probe;
  });
  return page.evaluate(
    async ({ b64, box, clip }) => {
      const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
      const bmp = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
      const cv = new OffscreenCanvas(bmp.width, bmp.height);
      const g = cv.getContext('2d')!;
      g.drawImage(bmp, 0, 0);
      const d = g.getImageData(0, 0, bmp.width, bmp.height).data;
      const k = bmp.width / clip.width;
      const y0 = Math.round((box.y - clip.y) * k);
      const y1 = Math.round((box.y - clip.y + box.h) * k);
      const x0 = Math.round((box.x - clip.x) * k);
      const x1 = Math.round((box.x - clip.x + box.w) * k);
      let top = -1;
      let bottom = -1;
      for (let y = y0; y < y1; y++) {
        for (let x = x0; x < x1; x++) {
          if (d[(y * bmp.width + x) * 4]! < 140) {
            if (top < 0) top = y;
            bottom = y;
            break;
          }
        }
      }
      // the ink's centre minus the box's centre, CSS px
      return top < 0 ? null : (top + bottom + 1) / 2 / k - (box.y - clip.y + box.h / 2);
    },
    { b64: png.toString('base64'), box, clip },
  );
}

/**
 * The vertical offsets of a control's label (and icon), in CSS px, centre of the label minus centre of the control. `cap`: the capitals of the label, as pixels
 * (the label's text set to "HHH" meanwhile: the box from the cap height to the baseline); `ink`: the real text's ink, descenders and all; `icon`: an svg or spinner.
 */
async function measure(page: Page, el: Locator, label = true): Promise<Measure> {
  const dom = await el.evaluate((node) => {
    const box = node.getBoundingClientRect();
    const w = document.createTreeWalker(node, NodeFilter.SHOW_TEXT, { acceptNode: (n) => (n.nodeValue && n.nodeValue.trim() ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT) });
    const tn = w.nextNode();
    const ic = node.querySelector('svg, .spinner');
    let icon: number | null = null;
    if (ic) {
      const b = ic.getBoundingClientRect();
      icon = b.top + b.height / 2 - (box.top + box.height / 2);
    }
    return { height: box.height, icon, font: tn ? getComputedStyle(tn.parentElement!).fontFamily : '', hasText: !!tn };
  });
  const off = (r: number | null) => (r === null ? null : +r.toFixed(2));
  const cap = dom.hasText && label ? off(await inkRows(page, el, 'HHH')) : null;
  const ink = dom.hasText && label ? off(await inkRows(page, el, null)) : null;
  return { cap, ink, icon: dom.icon === null ? null : +dom.icon.toFixed(2), height: +dom.height.toFixed(2), font: dom.font };
}

const supported = (page: Page) => page.evaluate(() => CSS.supports('text-box', 'trim-both cap alphabetic'));

async function check(page: Page, name: string, el: Locator): Promise<void> {
  await el.first().scrollIntoViewIfNeeded();
  const m = await measure(page, el.first());
  record(name, m);
  if (shots) {
    const b = (await el.first().boundingBox())!;
    mkdirSync(shots, { recursive: true });
    const pad = 8;
    await page.screenshot({ path: join(shots, `${tagged}-${name.replace(/[^a-z0-9]+/gi, '-')}.png`), clip: { x: Math.max(0, b.x - pad), y: Math.max(0, b.y - pad), width: b.width + 2 * pad, height: b.height + 2 * pad }, animations: 'disabled' });
  }
  if (m.cap !== null) expect.soft(Math.abs(m.cap), `${name}: the cap height of the label sits ${m.cap} px from the centre (font: ${m.font})`).toBeLessThanOrEqual(1);
  if (m.icon !== null) expect.soft(Math.abs(m.icon), `${name}: the icon sits ${m.icon} px from the centre`).toBeLessThanOrEqual(1);
}

test('the app: Connect, a small button, the status chip, a badge, the Rows control, the Activity filters', async ({ appPage: page }) => {
  await mockKascov(page, { tokens });
  await page.setViewportSize({ width: 1280, height: 800 });
  await forceFont(page);
  await page.goto('/#/market');
  await expect(page.getByTestId('token-list-rows-20')).toBeVisible();
  test.skip(!(await supported(page)), 'text-box is not supported by this browser');

  await check(page, 'header-connect-pill', page.getByTestId('wallet-connect-list').locator('.btn-pill').first());
  await check(page, 'header-btn-sm', page.getByTestId('wallet-connect-list').locator('.btn').last());
  await check(page, 'status-chip', page.getByTestId('network-chip'));
  await check(page, 'badge', page.locator('[data-testid^="token-badges-"] .badge').first());
  for (const n of [20, 50, 100]) await check(page, `rows-${n}`, page.getByTestId(`token-list-rows-${n}`));

  await page.goto('/#/activity');
  await expect(page.getByTestId('activity-actor')).toBeVisible();
  for (const a of ['all', 'orders', 'executions']) await check(page, `activity-actor-${a}`, page.getByTestId(`activity-actor-${a}`));

  // a busy button: the spinner takes the label's place, the box keeps its size; and a plain, a primary and a small `.btn` with the label as a span (Button.tsx)
  const busy = await page.evaluate(() => {
    const host = document.createElement('div');
    host.setAttribute('data-testid', 'btn-probe');
    host.style.cssText = 'position:fixed;left:20px;top:200px;z-index:9999;display:flex;gap:12px;padding:12px;background:var(--bg)';
    const mk = (id: string, cls: string, label: string, spin: boolean) => {
      const b = document.createElement('button');
      b.id = id;
      b.className = cls;
      if (spin) {
        const sp = document.createElement('span');
        sp.className = 'spinner';
        b.append(sp);
      }
      const l = document.createElement('span');
      l.className = 'btn-label';
      l.textContent = label;
      b.append(l);
      host.append(b);
    };
    mk('p-plain', 'btn', 'Place order', false);
    mk('p-primary', 'btn btn-primary', 'Place order', false);
    mk('p-sm', 'btn btn-sm', 'Cancel', false);
    mk('p-busy', 'btn btn-primary btn-loading btn-spin', 'Place order', true);
    document.body.append(host);
    const h = (id: string) => document.getElementById(id)!.getBoundingClientRect().height;
    return { plain: h('p-plain'), busy: h('p-busy') };
  });
  expect(busy.busy, 'a busy button keeps the height of its idle twin').toBeCloseTo(busy.plain, 1);
  await check(page, 'btn-plain', page.locator('#p-plain'));
  await check(page, 'btn-primary', page.locator('#p-primary'));
  await check(page, 'btn-sm', page.locator('#p-sm'));
  const spinner = await measure(page, page.locator('#p-busy'), false);
  record('btn-busy', spinner);
  expect(spinner.icon, 'the spinner of a busy button').not.toBeNull();
  expect(Math.abs(spinner.icon!), `the spinner sits ${spinner.icon} px from the centre`).toBeLessThanOrEqual(1);
  if (shots) {
    const b = (await page.locator('#p-busy').boundingBox())!;
    await page.screenshot({ path: join(shots, `${tagged}-btn-busy.png`), clip: { x: b.x - 8, y: b.y - 8, width: b.width + 16, height: b.height + 16 }, animations: 'disabled' });
  }
});

test('the landing page: the two calls to action, the chip, the header pill', async ({ appPage: page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await forceFont(page);
  await page.goto('/');
  await expect(page.getByTestId('landing')).toBeVisible();
  test.skip(!(await supported(page)), 'text-box is not supported by this browser');
  await check(page, 'landing-header-open-app', page.getByTestId('landing-open-app'));
  await check(page, 'landing-cta-solid', page.getByTestId('landing-cta'));
  await check(page, 'landing-cta-line', page.locator('.lp-pill-line').first());
  // the chip sits in the header's row on a wide screen and in the hero on a very small phone
  if (await page.getByTestId('landing-chip').isVisible()) await check(page, 'landing-chip', page.getByTestId('landing-chip'));
  await check(page, 'landing-header-chip', page.getByTestId('network-chip'));
});

test('a phone: the header Connect pill and a segmented control', async ({ appPage: page }) => {
  await mockKascov(page, { tokens });
  await page.setViewportSize({ width: 390, height: 844 });
  await forceFont(page);
  await page.goto('/#/market');
  await expect(page.getByTestId('wallet-connect')).toBeVisible();
  test.skip(!(await supported(page)), 'text-box is not supported by this browser');
  await check(page, 'phone-connect-pill', page.getByTestId('wallet-connect'));
  await check(page, 'phone-network-chip', page.getByTestId('network-chip'));
});
