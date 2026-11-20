// Token-logo votes (data/logo-votes.ts, LogoReport.tsx), against the mock stack: the mock indexer checks the Kaspa personal-message
// signature and the work of a vote as kobd does (mock/logo-votes.mjs). Two other wallets voted on a token's logo (1 up, 1 down); the
// connected wallet votes it down, which makes 3 voters at a net -1: the logo is hidden by default, with the letter and a "Hidden by votes"
// note, it can be shown once, and "Show all logos" in Settings shows it again. KOB_SHOTS=<dir> also saves screenshots of the vote UI and
// of the hidden logo.
import { join } from 'node:path';
import { expect, test } from '../fixtures';
import { connectWallet } from '../helpers/env';
import { chooseOption } from '../helpers/dropdown';
import { mockKascov, PNG, programHash, sha256 } from '../helpers/kascov';

test.use({ autoOpen: false });

const SAVED = 'a1'.repeat(32); // kascov.io's checked copy: a picture that hashes to its committed hash
const SHOTS = process.env.KOB_SHOTS;

test('a vote is signed and counted; 3 voters at net -1 hide the logo by default; it shows once, and "Show all logos" shows it', async ({ appPage: page, mock, wallet }) => {
  await mockKascov(page, {
    tokens: [{ covenant_id: SAVED, claimed_ticker: 'SAVED', claimed_name: 'Saved Icon Token', claimed_decimals: 3, claimed_image_hash: sha256(PNG), program: programHash('KaspaRocketToken11458') }],
    images: { [SAVED]: PNG },
  });
  // two other wallets voted already
  await mock.post('/mock/logo-votes', { covenant_id: SAVED, up: 1, down: 1 });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`/#/market/${SAVED}`);
  await connectWallet(page, 'kasware');

  const note = page.getByTestId('token-logo-note');
  await expect(note).toHaveAttribute('data-logo', 'checked');
  await expect(page.getByTestId('logo-votes-up')).toHaveText('1');
  await expect(page.getByTestId('logo-votes-down')).toHaveText('1');
  // what a vote is: behind the votes' "?", nothing of it on the page until it is opened
  await expect(page.getByTestId('logo-votes')).not.toContainText('signed message, no fee');
  await page.getByTestId('logo-vote-help').click();
  await expect(page.getByTestId('logo-vote-help-panel')).toContainText('signed message, no fee');
  await page.getByTestId('logo-vote-help').click();
  await expect(page.getByTestId('logo-vote-help-panel')).toHaveCount(0);
  const icon = page.getByTestId(`token-icon-${SAVED}`).first();
  await expect(icon).toHaveAttribute('data-icon', 'image');
  if (SHOTS) await page.getByTestId('token-logo-section').screenshot({ path: join(SHOTS, 'logo-votes-ui.png') });

  // the vote: the wallet signs the message, the page does the work, the mock indexer verifies both
  await page.getByTestId('logo-vote-down').click();
  await expect(page.getByTestId('logo-vote-result')).toContainText('Your vote is counted.');
  const calls = (await wallet!.calls()).filter((c) => c.method === 'signMessage');
  expect(calls).toHaveLength(1);
  const day = new Date().toISOString().slice(0, 10);
  expect((calls[0] as unknown as { message: string }).message).toBe(`KOB logo vote\nToken: ${SAVED}\nVote: down\nAddress: ${wallet!.config.addresses['testnet-10']}\nNetwork: testnet-10\nDay: ${day}`);
  const kept = await mock.get('/mock/logo-votes');
  expect(kept.votes).toHaveLength(1);
  expect(kept.votes[0]).toMatchObject({ covenant_id: SAVED, vote: 'down', voter: wallet!.config.pubkey });

  // 3 voters, net -1: hidden by default, the letter with its note, and no picture
  await expect(page.getByTestId('logo-votes-down')).toHaveText('2');
  await expect(note).toHaveAttribute('data-logo', 'voted');
  await expect(note).toContainText('Hidden by votes');
  await expect(icon).toHaveAttribute('data-icon', 'letter');
  await expect(icon).toHaveAttribute('data-logo-note', 'voted');
  await expect(icon).toHaveAttribute('title', 'Hidden by votes. Click to show it once.');
  await expect(icon.locator('img')).toHaveCount(0);
  // the report path stays as it was: the vote hid nothing for the report list
  expect(await page.evaluate(() => localStorage.getItem('kob.logo-blocklist.v1') ?? '')).not.toContain(SAVED);
  if (SHOTS) {
    await page.getByTestId('token-logo-section').screenshot({ path: join(SHOTS, 'logo-hidden-by-votes.png') });
    // the token's header: the letter with the small mark of a logo hidden by votes
    await page.evaluate(() => window.scrollTo(0, 0));
    await icon.hover();
    await page.screenshot({ path: join(SHOTS, 'logo-hidden-by-votes-header.png'), clip: { x: 0, y: 0, width: 1280, height: 260 } });
  }

  // a second vote the same day is refused: one vote per wallet per 24 hours
  await page.getByTestId('logo-vote-up').click();
  await expect(page.getByTestId('logo-vote-result')).toContainText('One vote a day per wallet');

  // shown once: the picture is back for this visit only
  await page.getByTestId('logo-voted-show').click();
  await expect(icon).toHaveAttribute('data-icon', 'image');
  await expect(note).toHaveAttribute('data-logo', 'checked');
  await page.reload();
  await expect(page.getByTestId('token-logo-note')).toHaveAttribute('data-logo', 'voted');

  // "Show all logos" in Settings
  await page.goto('/#/settings');
  const choice = page.getByTestId('settings-icons-votes');
  await expect(choice).toContainText('Show logos at 0 and above');
  await chooseOption(choice, 'all');
  expect(await page.evaluate(() => localStorage.getItem('kob.icons.votes'))).toBe('0');
  if (SHOTS) await page.getByTestId('settings-icons').screenshot({ path: join(SHOTS, 'logo-votes-setting.png') });
  await page.goto(`/#/market/${SAVED}`);
  await expect(page.getByTestId('token-logo-note')).toHaveAttribute('data-logo', 'checked');
  await expect(page.getByTestId(`token-icon-${SAVED}`).first()).toHaveAttribute('data-icon', 'image');
});
