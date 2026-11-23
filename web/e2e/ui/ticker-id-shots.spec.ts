// Screenshots only (R3B-L1): the Portfolio's open orders with a token whose ticker reads as KAS in
// other letters, at desktop and phone width. The files go to KOB_SHOTS as
// r3-portfolio-<KOB_SHOT_TAG>-<view>.png. Skipped without KOB_SHOTS.
import { test } from '../fixtures';
import { TOKEN, connectWallet } from '../helpers/env';
import { seedPortfolio } from '../helpers/portfolio-seed';

test.use({ autoOpen: false });
test.skip(!process.env.KOB_SHOTS, 'screenshots only');

const LOOK = 'c5'.repeat(32);
const VIEWS = [
  { id: 'desktop', width: 1280 },
  { id: 'phone', width: 390 },
] as const;

for (const v of VIEWS) {
  test(`portfolio ticker shots ${v.id}`, async ({ appPage: page, mock }) => {
    test.setTimeout(180_000);
    await page.setViewportSize({ width: v.width, height: 1000 });
    await seedPortfolio(mock);
    await mock.seed({
      tokens: [{ ticker: 'ᏦᎪᏚ', covenant_id: LOOK, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 100_000_000 }],
      orders: [{ token: LOOK, side: 'ask', maker: 'alice', amount: (1_000n * TOKEN).toString(), price: 1_000_000 }],
    });
    await page.goto('/#/portfolio/open');
    await connectWallet(page, 'kasware');
    await page.getByTestId('orders-list').waitFor();
    await page.waitForTimeout(2000);
    await page.screenshot({ path: `${process.env.KOB_SHOTS}/r3-portfolio-${process.env.KOB_SHOT_TAG}-${v.id}.png`, fullPage: true });
  });
}
