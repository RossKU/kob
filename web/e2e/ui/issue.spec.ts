// Smoke tests of the token creation view (#/create; the old #/issue leads there) against the mock stack: a full creation (KCC-23: the
// preparation declaring the token details, then the genesis spending it) with the mock KasWare wallet and with the private key (one approval in
// the signer frame), and the blocking cases.
import { test, expect, TEST_KEYS } from '../fixtures';
import { readReceipt, confirmReceipt, fillFields, pickType, placeOrder, waitReviewable } from '../helpers/ticket';

const KAS = 100_000_000n;

test.use({ autoOpen: false });

test.describe('token issuance', () => {
  test('issues a fixed-supply token: form, review, confirmation, signing, result panel with the registry entry', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await expect(page.getByTestId('issue-form')).toBeVisible();
    await expect(page.getByTestId('issue-review')).toBeDisabled();
    // the page is "Create a token": the basics in view, one plain line on what the program can do, the rest folded under Advanced
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Create a token');
    await expect(page.getByTestId('nav-create')).toHaveText('Create token');
    await expect(page.getByTestId('issue-fixed-line')).toHaveText('Fixed supply: no one can mint more, freeze or seize these tokens.');
    for (const id of ['issue-name', 'issue-ticker', 'issue-supply', 'issue-decimals', 'issue-icon', 'issue-description', 'issue-website', 'issue-x', 'issue-telegram']) await expect(page.getByTestId(id), id).toBeVisible();
    await expect(page.getByTestId('issue-decimals')).toHaveValue('8');
    await expect(page.getByTestId('issue-holder-add')).toBeHidden();
    await expect(page.getByTestId('issue-carrier')).toBeHidden();
    await page.getByTestId('issue-advanced').getByRole('button').first().click();
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
    // the token's accounts: a handle each, kept as the account's address in the proposed registry entry
    await page.getByTestId('issue-x').fill('@newkcc');
    await page.getByTestId('issue-telegram').fill('@newkcc_chat');

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
    // what is recorded on chain with the token (KCC-23), and that this wallet asks twice
    const declare = page.getByTestId('confirm-section-declare');
    await expect(declare).toBeVisible();
    await expect(page.getByTestId('confirm-declare-declare-name')).toContainText('New Example Coin');
    await expect(page.getByTestId('confirm-declare-declare-symbol')).toContainText('NEWKCC');
    await expect(page.getByTestId('confirm-declare-declare-decimals')).toContainText('2');
    await expect(page.getByTestId('confirm-declare-declare-x')).toContainText('https://x.com/newkcc');
    await expect(page.getByTestId('confirm-info').locator('[data-code="walletTwice"]')).toHaveCount(1);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();

    // the pre-sign screen reports the accepted transaction; closing it hands the result to the issuance view
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();

    // result panel
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    const tokenId = await page.getByTestId('issue-result-tokenId').getAttribute('data-value');
    expect(tokenId).toMatch(/^[0-9a-f]{64}$/);

    // the mock node accepted the preparation, then the genesis spending its output 0; the covenant is the one shown
    const submitted = await mock.submitted();
    expect(submitted.length).toBeGreaterThan(1);
    const genesis = submitted[submitted.length - 1];
    const prep = submitted[submitted.length - 2];
    expect(genesis.txid).toBe(await page.getByTestId('issue-result-txid').getAttribute('data-value'));
    expect(prep.txid).toBe(await page.getByTestId('issue-result-prepTxid').getAttribute('data-value'));
    expect(genesis.tx.inputs.map((i: any) => [i.transactionId, i.index])).toEqual([[prep.txid, 0]]);
    // the genesis names the states of its token outputs (KOBG | version 1 | count | length-prefixed 112-byte state spans): how an indexer, the
    // issuing key in another browser and the CLI find the issued output, which has no transfer leader to reveal it
    expect(genesis.tx.payload).toMatch(/^4b4f4247010170[0-9a-f]{224}$/);
    const declared = '{"name":"New Example Coin","symbol":"NEWKCC","decimals":2,"x":"https://x.com/newkcc","telegram":"https://t.me/newkcc_chat"}';
    expect(prep.tx.payload).toBe(`00${Buffer.from(declared, 'utf8').toString('hex')}`);
    const covenantIds = (genesis.tx?.outputs ?? []).map((o: any) => o?.covenant?.covenantId ?? o?.covenant?.covenant_id).filter(Boolean);
    expect(covenantIds).toEqual([tokenId]);
    // the indexer captured the declaration (as the executor does) and serves it as a launch proof; the result offers the same proof
    const proof = await mock.get(`/v1/tokens/${tokenId}/declaration`);
    expect(proof).toMatchObject({ format: 'kob-kcc23-proof/1', covenantId: tokenId, preparation: { payload: prep.tx.payload } });
    await expect(page.getByTestId('issue-proof-download')).toBeVisible();
    // nothing is left to resume
    expect(await page.evaluate(() => localStorage.getItem('kob.creations.v1'))).toBeNull();

    // the wallet's token output is remembered by the token tracker (test hook window.__kob, present with features.test)
    const tracked = await page.evaluate((pk) => (window as any).__kob.services.tracker.list(pk), TEST_KEYS.alice.pubkey);
    expect(tracked).toHaveLength(1);
    expect(tracked[0]).toMatchObject({ transactionId: genesis.txid, index: 0, tokenCovId: tokenId, program: 'KCC20Ref' });
    expect(tracked[0].state.amount).toBe('100000000');

    // the token's facts in the registry format (for tools and community lists: no listing request; the page itself shows no verification state)
    const entry = JSON.parse((await page.getByTestId('issue-registry-entry').textContent()) ?? '');
    expect(entry).toMatchObject({ ticker: 'NEWKCC', name: 'New Example Coin', decimals: 2, covenant_id: tokenId, template_id: 'kcc20-ref-3x3', max_token_inputs: 3, verified: false });
    expect(entry).not.toHaveProperty('status');
    expect(entry.display).toMatchObject({ x: 'https://x.com/newkcc', telegram: 'https://t.me/newkcc_chat' });
    for (const gone of ['issue-result-unverified', 'issue-result-pending', 'issue-result-nottradable']) await expect(page.getByTestId(gone)).toHaveCount(0);
    await expect(page.getByTestId('issue-result')).not.toContainText(/unverified|not tradable/i);
    await expect(page.getByTestId('issue-result-listing-note')).toContainText('KOB keeps no list of approved tokens');
    await expect(page.getByTestId('issue-result-listing-note')).not.toContainText(/pull request|official/i);

    // download of the entry
    const [download] = await Promise.all([page.waitForEvent('download'), page.getByTestId('issue-registry-download').click()]);
    expect(download.suggestedFilename()).toBe('NEWKCC.registry-entry.json');

    // reset for another issuance
    await page.getByTestId('issue-another').click();
    await expect(page.getByTestId('issue-form')).toBeVisible();
    await expect(page.getByTestId('issue-name')).toHaveValue('');

    // its market (no order yet: known from the wallet's holdings) names it by its declaration: the declared name and ticker, marked as declared
    // on chain like the wallet views, and the banner says the decimals are the declared ones (not a price scale, not "the chain carries none")
    await page.goto(`/#/market/${tokenId}`);
    const header = page.getByTestId('token-header');
    await expect(header.getByTestId('token-name')).toHaveText('New Example Coin');
    await expect(header.getByTestId('token-title')).toContainText('NEWKCC');
    await expect(header.getByTestId('token-decimals-note')).toHaveAttribute('data-source', 'declared');
    await expect(header.getByTestId('token-decimals-note')).toContainText('declared, bound by ancestry');
    await expect(header.getByTestId('badge-family')).toHaveAttribute('title', /whole tokens of 2 decimals, as its creator declared at creation/);
    await expect(header).not.toContainText('carries no decimals');
  });

  test('a token created in one browser is found, listed and sold from a fresh profile of the same key', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    await page.getByTestId('issue-name').fill('Fresh Profile Coin');
    await page.getByTestId('issue-ticker').fill('FRESH');
    await page.getByTestId('issue-supply').fill('1000');
    await expect(page.getByTestId('issue-holder-0-amount')).toHaveValue('1000');
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    const tokenId = (await page.getByTestId('issue-result-tokenId').getAttribute('data-value'))!;
    const genesisTxid = (await page.getByTestId('issue-result-txid').getAttribute('data-value'))!;

    // the issuing browser tracks the output; the INDEXER lists it too, to the owner, before any transfer or order (the genesis named it)
    const alice = TEST_KEYS.alice.pubkey;
    expect(await page.evaluate((pk) => (window as any).__kob.services.tracker.list(pk).length, alice)).toBe(1);
    const held = await mock.get(`/v1/token-utxos?owner=${alice}&token=${tokenId}`);
    expect(held.items).toHaveLength(1);
    expect(held.items[0]).toMatchObject({ txid: genesisTxid, index: 0, token: tokenId, owner: alice, amount: '100000000000', role: 'owned', program: 'KCC20Ref', spent: false });
    // a key that did not receive it is listed nothing
    expect((await mock.get(`/v1/token-utxos?owner=${TEST_KEYS.bob.pubkey}&token=${tokenId}`)).items).toEqual([]);

    // a fresh profile: this browser forgets everything it knew (the tracker, the creation records, the settings)
    await page.goto('about:blank');
    await page.evaluate(() => undefined);
    await page.goto('/#/create');
    await page.evaluate(() => {
      localStorage.clear();
      sessionStorage.clear();
    });
    await page.goto(`/#/market/${tokenId}`);
    await page.reload();
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    expect(await page.evaluate((pk) => (window as any).__kob.services.tracker.list(pk).length, alice)).toBe(0);

    // the wallet holds the tokens (found through the indexer and confirmed on the node) and can list them for sale
    await expect(page.getByTestId('order-ticket')).toBeVisible({ timeout: 30_000 });
    await pickType(page, 'limit', 'sell');
    await fillFields(page, { amount: '100', price: '0.05' });
    await waitReviewable(page);
    const placed = await placeOrder(page, mock);
    expect(placed.submission.created).toHaveLength(1);
    const asks = await mock.ordersOf('alice');
    expect(asks).toHaveLength(1);
    expect(asks[0]).toMatchObject({ token: tokenId, side: 1, status: 'open' }); // side 1: an ask (a sell)
    // and the order took the very output the genesis made
    expect(placed.submission.spent?.map((s) => s.outpoint)).toContain(`${genesisTxid}:0`);
  });

  test('Settings: "Find a token you created earlier" recovers an issuance output of an older creation this browser never saw', async ({ appPage: page, mock }) => {
    // a token created before genesis payloads named their states: its output is held by the key, the app knows nothing of it (no tracker entry,
    // no declaration, decimals unknown). The issuer says what the creation gave the key; the node confirms the output.
    const id = '9c'.repeat(32);
    await mock.seed({ tokens: [{ ticker: '', covenant_id: id, program: 'KCC20Ref', extension_commitment: null, decimals: null, hidden: true }] });
    await mock.giveTokens('alice', 500_000n, { token: id });
    await page.goto('/#/settings');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    await expect(page.getByTestId('tracker-empty')).toBeVisible();
    // the backup is named where it is made
    await expect(page.getByTestId('settings-tracker')).toContainText('Keep this list');

    await page.getByTestId('recover-id').fill('not an id');
    await page.getByTestId('recover-amount').fill('5000');
    await page.getByTestId('recover-find').click();
    await expect(page.getByTestId('recover-message')).toContainText('64-character covenant id');
    await page.getByTestId('recover-id').fill(id);
    await page.getByTestId('recover-find').click();
    // the token declares no decimals on chain: they are asked for
    await expect(page.getByTestId('recover-message')).toContainText('declares no decimals');
    await page.getByTestId('recover-decimals').fill('2');
    // an amount the creation did not give the key finds nothing, and nothing is stored
    await page.getByTestId('recover-amount').fill('4999');
    await page.getByTestId('recover-find').click();
    await expect(page.getByTestId('recover-message')).toContainText('holds no output of this token for your key with that amount');
    await expect(page.getByTestId('tracker-empty')).toBeVisible();
    // the exact amount: found, confirmed on the node, remembered
    await page.getByTestId('recover-amount').fill('5000');
    await page.getByTestId('recover-find').click();
    await expect(page.getByTestId('recover-message')).toContainText('Found 1 output(s) of this token for your key.');
    await expect(page.getByTestId('tracker-row')).toHaveCount(1);
    const tracked = await page.evaluate((pk) => (window as any).__kob.services.tracker.list(pk), TEST_KEYS.alice.pubkey);
    expect(tracked).toHaveLength(1);
    expect(tracked[0]).toMatchObject({ tokenCovId: id, program: 'KCC20Ref' });
    expect(tracked[0].state.amount).toBe('500000');
  });

  test('a ticker another registry token uses is a warning, never a block (the covenant id is the identity)', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await page.getByTestId('issue-name').fill('Second Example');
    await page.getByTestId('issue-ticker').fill('exkcc');
    await page.getByTestId('issue-supply').fill('1000');
    await expect(page.getByTestId('issue-issue-ticker.exists')).toContainText('the covenant id tells them apart');
    await expect(page.getByTestId('issue-ticker')).not.toHaveAttribute('aria-invalid', 'true');
    await expect(page.getByTestId('issue-review')).toBeEnabled();

    // a look-alike ticker warns the same way; a different one has no warning
    await page.getByTestId('issue-ticker').fill('EXKR0N'); // EXKRON with a zero
    await expect(page.getByTestId('issue-issue-ticker.lookalike')).toBeVisible();
    await expect(page.getByTestId('issue-review')).toBeEnabled();
    await page.getByTestId('issue-ticker').fill('OKTKN');
    await expect(page.getByTestId('issue-issue-ticker.exists')).toHaveCount(0);
    await expect(page.getByTestId('issue-issue-ticker.lookalike')).toHaveCount(0);
    await expect(page.getByTestId('issue-review')).toBeEnabled();
  });

  test('holders that exceed the supply block the review; the wallet share is the remainder', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();

    await page.getByTestId('issue-name').fill('Split Coin');
    await page.getByTestId('issue-ticker').fill('SPLIT');
    await page.getByTestId('issue-decimals').fill('0');
    await page.getByTestId('issue-supply').fill('100');
    // the holders are under Advanced
    await page.getByTestId('issue-advanced').getByRole('button').first().click();
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
    await page.goto('/#/create');
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

  test('a creation whose genesis did not go out is offered again and sent without a new signature', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-kasware').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    await page.getByTestId('issue-name').fill('Resume Coin');
    await page.getByTestId('issue-ticker').fill('RESUME');
    await page.getByTestId('issue-supply').fill('100');
    await expect(page.getByTestId('issue-summary')).toBeVisible();
    // the node takes the preparation and refuses the genesis: the app's fetch of the second submission fails
    let submits = 0;
    await page.route('**/*', async (route) => {
      const r = route.request();
      if (r.method() === 'POST' && (r.postData() ?? '').includes('"payload":"4b4f4247') && /submit/i.test(r.url() + (r.postData() ?? '')) && ++submits === 1) {
        return route.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ error: 'Rejected transaction x: the node is not taking it now (invalid)' }) });
      }
      return route.continue();
    });
    await page.getByTestId('issue-review').click();
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    await expect(page.getByTestId('confirm-error')).toContainText('stays in your wallet', { timeout: 30_000 });
    await page.getByTestId('confirm-replan').click();
    await page.unroute('**/*');
    // only the preparation went out; the creation is offered again
    const before = await mock.submitted();
    expect(Buffer.from(before[before.length - 1]!.tx.payload.slice(2), 'hex').toString()).toContain('"symbol":"RESUME"');
    const pending = page.getByTestId('issue-pending');
    await expect(pending).toBeVisible();
    await expect(pending).toContainText('RESUME');
    await page.getByTestId('issue-pending-send').click();
    await expect(page.getByTestId('issue-result')).toBeVisible({ timeout: 30_000 });
    const after = await mock.submitted();
    expect(after.length).toBe(before.length + 1);
    expect(after[after.length - 1]!.tx.inputs[0].transactionId).toBe(before[before.length - 1]!.txid);
    await expect(page.getByTestId('issue-pending')).toHaveCount(0);
  });

  test('without a wallet the view asks for a connection and does not plan', async ({ appPage: page }) => {
    await page.goto('/#/create');
    await expect(page.getByTestId('issue-connect')).toBeVisible();
    await page.getByTestId('issue-name').fill('No Wallet');
    await page.getByTestId('issue-ticker').fill('NOWAL');
    await page.getByTestId('issue-supply').fill('10');
    await expect(page.getByTestId('issue-review')).toBeDisabled();
  });
});

test.describe('token creation with the private key', () => {
  test.use({ walletId: null });

  test('one approval in the signer frame signs both transactions; the frame names what is recorded on chain', async ({ appPage: page, mock }) => {
    await mock.giveKas('alice', 500n * KAS);
    await page.goto('/#/create');
    await page.getByTestId('wallet-connect-local-key').click();
    const f = page.frameLocator('[data-testid="signer-frame"]');
    await f.getByTestId('signer-key').fill(TEST_KEYS.alice.secretKey);
    await f.getByTestId('signer-connect').click();
    await expect(page.getByTestId('wallet-address')).toBeVisible();
    await page.getByTestId('issue-name').fill('Key Coin');
    await page.getByTestId('issue-ticker').fill('KEYC');
    await page.getByTestId('issue-decimals').fill('4');
    await page.getByTestId('issue-supply').fill('250');
    await page.getByTestId('issue-review').click();
    await expect(page.getByTestId('confirm-screen')).toBeVisible();
    await expect(page.getByTestId('confirm-info').locator('[data-code="walletTwice"]')).toHaveCount(0);
    await page.getByTestId('confirm-ack').check();
    await page.getByTestId('confirm-sign').click();
    // the frame: ONE request for the creation, its declared facts and the genesis checked against the claimed states
    const receipt = await readReceipt(page);
    expect(receipt.title).toBe('Create token');
    expect(receipt.lines['Name (recorded on chain)']).toBe('Key Coin');
    expect(receipt.lines['Ticker (recorded on chain)']).toBe('KEYC');
    expect(receipt.lines['Decimals (recorded on chain)']).toBe('4');
    expect(receipt.lines['Total supply']).toBe('250 KEYC');
    await expect(f.getByTestId('signer-problems')).toHaveCount(0);
    await confirmReceipt(page);
    await expect(page.getByTestId('tx-status')).toBeVisible({ timeout: 30_000 });
    // no second request: both went out
    await expect(page.getByTestId('signer-overlay')).toBeHidden();
    const submitted = await mock.submitted();
    const genesis = submitted[submitted.length - 1]!;
    const prep = submitted[submitted.length - 2]!;
    expect(genesis.tx.inputs[0].transactionId).toBe(prep.txid);
    expect(Buffer.from(prep.tx.payload.slice(2), 'hex').toString()).toBe('{"name":"Key Coin","symbol":"KEYC","decimals":4}');
  });
});
