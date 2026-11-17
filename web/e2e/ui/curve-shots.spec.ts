// Screenshots only: the book and Open orders with a curve sell order of the wallet (KOB_CURVE=1; without it, the same page before curves), desktop
// light, desktop dark and a phone, to KOB_SHOTS with the prefix KOB_SHOT_TAG. Skipped without KOB_SHOTS.
import { test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });
test.skip(!process.env.KOB_SHOTS, 'screenshots only');

const VIEWS = [
  { id: 'light', width: 1280, theme: 'light' },
  { id: 'dark', width: 1280, theme: 'dark' },
  { id: 'phone', width: 390, theme: 'light' },
] as const;

for (const v of VIEWS) {
  test(`curve shots ${v.id}`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: v.width, height: 1000 });
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), v.theme);
    await seedPortfolio(mock);
    const tok = await mock.token();
    let curveId: string | null = null;
    if (process.env.KOB_CURVE === '1') {
      // a 20-level curve of 100 EXKCC (2.55 KAS if sold out, last level 1.04x the first: its levels among the plain asks), 3 levels sold
      await mock.seed({ orders: [{ token: tok.covenant_id, kind: 'KobCurveAsk', maker: 'alice', amount: '10000000000', levels: 20, total: '255000000', multiple: '1.04' }] });
      const curve = (await mock.ordersOf('alice')).find((o: { contract: string }) => o.contract === 'KobCurveAsk')!.covenant_id as string;
      await mock.fill(curve, 1_500_000_000n);
      curveId = curve;
    }
    await listExkcc(page);
    const name = (n: string) => `${process.env.KOB_SHOTS}/${n}-${process.env.KOB_SHOT_TAG}-${v.id}.png`;
    await page.goto(`/#/market/${tok.covenant_id}`);
    await connectWallet(page, 'kasware');
    const book = page.getByTestId('order-book').first();
    await book.waitFor();
    await page.waitForTimeout(2000);
    await book.screenshot({ path: name('book') });
    await page.goto('/#/portfolio/open');
    await page.getByTestId('orders-list').waitFor();
    await page.waitForTimeout(1500);
    await page.screenshot({ path: name('orders'), fullPage: true });
    if (curveId) {
      // the curve's details: the levels left as a step line
      await page.getByTestId(`order-toggle-${curveId}`).click();
      const details = page.getByTestId(`order-details-${curveId}`);
      await details.waitFor();
      await page.waitForTimeout(500);
      await details.screenshot({ path: name('details') });
    }
  });
}
