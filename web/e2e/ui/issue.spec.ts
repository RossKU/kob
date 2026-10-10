// Smoke tests of the issuance view (#/issue) against the mock stack: a full issuance with the mock KasWare wallet, and the blocking cases.
import { test, expect, TEST_KEYS } from '../fixtures';

const KAS = 100_000_000n;

test.use({ autoOpen: false });

test.describe('token issuance', () => {
  test('issues a fixed-supply token: form, review, confirmation, signing, result panel with the registry entry', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/issue');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await expect(page.getByTestId('issue-form')).toBeVisible();
    await expect(page.getByTestId('issue-review')).toBeDisabled();
    // the program: KOB's standard 3 / 3 reference by default, the published public-mint build as the one alternative
    await expect(page.getByTestId('issue-program-standard')).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId('issue-program').getByRole('radio')).toHaveCount(2);
    await expect(page.getByTestId('issue-fixed-program')).toContainText('KCC20Ref');
    await page.getByTestId('issue-name').fill('New Example Coin');
    // the ticker is uppercased as typed
    await page.getByTestId('issue-ticker').fill('newkcc');
    await expect(page.getByTestId('issue-ticker')).toHaveValue('NEWKCC');
    await page.getByTestId('issue-decimals').fill('2');
    await page.getByTestId('issue-supply').fill('1000000');

    // default holder: the whole supply to the connected wallet, shown explicitly
    await expect(page.getByTestId('issue-holder-0-amount')).toHaveValue('1000000');
    await expect(page.getByTestId('issue-summary')).toBeVisible();
    await expect(page.getByTestId('issue-summary-supply')).toContainText('1,000,000');
    await expect(page.getByTestId('issue-summary')).toContainText('KCC20Ref');
    // switching to the public-mint build plans that program; back to the standard one for the issuance below
    await page.getByTestId('issue-program-public-mint').click();
    await expect(page.getByTestId('issue-program-public-mint')).toHaveAttribute('aria-checked', 'true');
    await expect(page.getByTestId('issue-summary')).toContainText('KCC20PublicMint');
    await expect(page.getByTestId('issue-fixed-program')).toContainText('KCC20PublicMint');
    await page.getByTestId('issue-program-standard').click();
    await expect(page.getByTestId('issue-summary')).not.toContainText('KCC20PublicMint');

    await page.getByTestId('issue-review').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();

    // the pre-sign screen reports the accepted transaction; closing it hands the result to the issuance view
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();

    // result panel
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    const tokenId = await page.getByTestId('issue-result-tokenId').getAttribute('data-value');
    expect(tokenId).toMatch(/^[0-9a-f]{64}$/);

    // the mock node accepted the genesis, and its covenant is the one shown
    const submitted = await mock.submitted();
    expect(submitted.length).toBeGreaterThan(0);
    const genesis = submitted[submitted.length - 1];
    expect(genesis.txid).toBe(await page.getByTestId('issue-result-txid').getAttribute('data-value'));
    const covenantIds = (genesis.tx?.outputs ?? []).map((o: any) => o?.covenant?.covenantId ?? o?.covenant?.covenant_id).filter(Boolean);
    expect(covenantIds).toEqual([tokenId]);

    // the wallet's token output is remembered by the token tracker (test hook window.__kob, present with features.test)
    const tracked = await page.evaluate((pk) => (window as any).__kob.services.tracker.list(pk), TEST_KEYS.alice.pubkey);
    expect(tracked).toHaveLength(1);
    expect(tracked[0]).toMatchObject({ transactionId: genesis.txid, index: 0, tokenCovId: tokenId, program: 'KCC20Ref' });
    expect(tracked[0].state.amount).toBe('100000000');

    // registry entry: pending review, unverified
    const entry = JSON.parse((await page.getByTestId('issue-registry-entry').textContent()) ?? '');
    expect(entry).toMatchObject({ ticker: 'NEWKCC', name: 'New Example Coin', decimals: 2, covenant_id: tokenId, template_id: 'kcc20-ref-3x3', max_token_inputs: 3, status: 'pending-review', verified: false });
    await expect(page.getByTestId('issue-result-unverified')).toBeVisible();
    await expect(page.getByTestId('issue-result-listing-note')).toContainText('registry/tokens.json');

    // download of the entry
    const [download] = await Promise.all([page.waitForEvent('download'), page.getByTestId('issue-registry-download').click()]);
    expect(download.suggestedFilename()).toBe('NEWKCC.registry-entry.json');

    // reset for another issuance
    await page.getByTestId('issue-another').click();
    await expect(page.getByTestId('issue-form')).toBeVisible();
    await expect(page.getByTestId('issue-name')).toHaveValue('');
  });

  test('a ticker that already exists in the registry blocks the review', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/issue');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await page.getByTestId('issue-name').fill('Impostor');
    await page.getByTestId('issue-ticker').fill('exkcc');
    await page.getByTestId('issue-supply').fill('1000');
    await expect(page.getByTestId('issue-ticker')).toHaveAttribute('aria-invalid', 'true');
    await expect(page.getByTestId('issue-review')).toBeDisabled();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);

    // a different ticker clears the block
    await page.getByTestId('issue-ticker').fill('OKTKN');
    await expect(page.getByTestId('issue-ticker')).not.toHaveAttribute('aria-invalid', 'true');
    await expect(page.getByTestId('issue-review')).toBeEnabled();
  });

  test('holders that exceed the supply block the review; the wallet share is the remainder', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/issue');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await page.getByTestId('issue-name').fill('Split Coin');
    await page.getByTestId('issue-ticker').fill('SPLIT');
    await page.getByTestId('issue-decimals').fill('0');
    await page.getByTestId('issue-supply').fill('100');
    await page.getByTestId('issue-holder-add').click();
    await page.getByTestId('issue-holder-1-owner').fill('ab'.repeat(32));
    await page.getByTestId('issue-holder-1-amount').fill('30');
    await expect(page.getByTestId('issue-holder-0-amount')).toHaveValue('70');
    await expect(page.getByTestId('issue-review')).toBeEnabled();

    await page.getByTestId('issue-holder-1-amount').fill('130');
    await expect(page.getByTestId('issue-issue-holders.exceeds')).toBeVisible();
    await expect(page.getByTestId('issue-review')).toBeDisabled();

    await page.getByTestId('issue-holder-1-remove').click();
    await expect(page.getByTestId('issue-holder-1-owner')).toHaveCount(0);
    await expect(page.getByTestId('issue-holder-0-amount')).toHaveValue('100');
    await expect(page.getByTestId('issue-review')).toBeEnabled();
  });

  test('a wallet without enough KAS sees the exact shortfall and cannot review', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 4n * KAS);
    await page.goto('/#/issue');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await page.getByTestId('issue-name').fill('Poor Coin');
    await page.getByTestId('issue-ticker').fill('POOR');
    await page.getByTestId('issue-supply').fill('1000');
    // 10 KAS carrier default, 4 KAS in the wallet: 6 KAS short
    const shortfall = page.getByTestId('issue-issue-funds.insufficient');
    await expect(shortfall).toBeVisible();
    await expect(shortfall).toContainText('6');
    await expect(page.getByTestId('issue-review')).toBeDisabled();
  });

  test('without a wallet the view asks for a connection and does not plan', async ({ appPage: page }) => {
    await page.goto('/#/issue');
    await expect(page.getByTestId('issue-connect')).toBeVisible();
    await page.getByTestId('issue-name').fill('No Wallet');
    await page.getByTestId('issue-ticker').fill('NOWAL');
    await page.getByTestId('issue-supply').fill('10');
    await expect(page.getByTestId('issue-review')).toBeDisabled();
  });
});
