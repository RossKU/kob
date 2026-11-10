// Screenshots only (not a test of anything): every tab of the Portfolio and a market page's panel at 390, 600, 720, 1024 and 1440 px in the theme KOB_THEME, to KOB_SHOTS
// with the prefix KOB_SHOT_TAG. Skipped without KOB_SHOTS.
import { test } from '../fixtures';
import { listExkcc, connectWallet } from '../helpers/env';
import { lateTimeZone, seedLongValues } from '../helpers/portfolio-long';
import { seedManyDone, seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false, timezoneId: lateTimeZone() });
test.skip(!process.env.KOB_SHOTS, 'screenshots only');

for (const width of [390, 600, 720, 1024, 1440]) {
  test(`shots at ${width}`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width, height: 1100 });
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), process.env.KOB_THEME ?? 'dark');
    await seedPortfolio(mock);
    await seedLongValues(mock);
    await seedManyDone(mock, 3);
    await listExkcc(page);
    await page.goto('/#/portfolio');
    await connectWallet(page, 'kasware');
    const name = (n: string) => `${process.env.KOB_SHOTS}/${process.env.KOB_SHOT_TAG}-${process.env.KOB_THEME ?? 'dark'}-${width}-${n}.png`;
    for (const id of ['balances', 'active', 'history', 'fills']) {
      await page.getByTestId(`orders-tab-${id}`).click();
      await page.waitForTimeout(1500);
      await page.screenshot({ path: name(id), fullPage: true });
    }
    const token = await mock.token();
    await page.goto(`/#/market/${token.covenant_id}`);
    const mine = page.getByTestId('market-orders');
    await mine.getByTestId('orders-tab-history').click();
    await page.waitForTimeout(1500);
    await mine.scrollIntoViewIfNeeded();
    await mine.screenshot({ path: name('market-history') });
  });
}
