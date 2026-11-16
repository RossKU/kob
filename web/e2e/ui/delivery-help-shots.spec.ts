// Screenshots and a video of the Delivery help (DeliveryHelp.tsx) for review: desktop light mid-animation with one loop on video, desktop dark,
// phone light. Not a test of anything; skipped without KOB_DELIVERY_SHOTS=<dir>.
import { expect, test } from '../fixtures';
import { pickType } from '../helpers/ticket';
import { openStd } from '../helpers/delivery';

const shots = process.env.KOB_DELIVERY_SHOTS;
test.use({ autoOpen: false, video: shots ? { mode: 'on', size: { width: 1280, height: 900 } } : 'off' });
test.skip(!shots, 'screenshots only (KOB_DELIVERY_SHOTS)');

const openAdvanced = async (page: import('@playwright/test').Page): Promise<void> => {
  const box = page.getByTestId('order-advanced');
  if ((await box.getAttribute('open')) === null) await box.locator('summary').click();
};

for (const v of [{ theme: 'light', width: 1280, name: 'help-desktop-light' }, { theme: 'dark', width: 1280, name: 'help-desktop-dark' }, { theme: 'light', width: 390, name: 'help-phone-light' }]) {
  test(`help shot ${v.name}`, async ({ appPage: page, mock }) => {
    test.setTimeout(90_000);
    await page.setViewportSize({ width: v.width, height: v.width < 600 ? 844 : 900 });
    await page.addInitScript((th) => localStorage.setItem('kob.theme', th), v.theme);
    await openStd(page, mock);
    await pickType(page, 'limit', 'buy');
    await openAdvanced(page);
    await page.getByTestId('order-delivery-help').click();
    const help = page.getByTestId('delivery-help');
    const caption = help.getByTestId('delivery-help-caption');
    if (v.name === 'help-desktop-light') {
      // one whole loop on the video, then the shot mid-animation
      await expect(caption).toHaveText('Order ends', { timeout: 10_000 });
      await expect(caption).toHaveText('Fill 2 of 3', { timeout: 10_000 });
      await page.waitForTimeout(500);
    } else {
      await expect(caption).toHaveText('Order ends', { timeout: 10_000 });
      await help.getByTestId('delivery-help-play').click();
      await page.waitForTimeout(800);
    }
    // a phone shows the dialog as it scrolls on screen; a desktop the whole dialog
    if (v.width < 600) await page.screenshot({ path: `${shots}/delivery-${v.name}.png` });
    else await help.screenshot({ path: `${shots}/delivery-${v.name}.png` });
    const video = page.video();
    if (v.name === 'help-desktop-light' && video) {
      await page.close();
      await video.saveAs(`${shots}/delivery-help-loop.webm`);
    }
  });
}
