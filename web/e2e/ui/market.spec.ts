// Smoke specs of the shell and the market views against the mock stack (seeded book: token EXKCC, 10 levels per side, 8 fills).
// Per-order-type specs belong to the ticket work; here: the list, the token page, live status, wallet, lookalike protection, health banners.
import { expect, test, TEST_KEYS } from '../fixtures';
import { TESTID } from '../testids';

/** The seeded token as the mock indexer reports it (prices: sompi per whole token, i.e. per 1e8 base units). */
async function seeded(mock: import('../fixtures').MockClient) {
  const tok = await mock.token();
  return { id: tok.covenant_id, short: `${tok.covenant_id.slice(0, 4)}…${tok.covenant_id.slice(-4)}` };
}

// `/` is the landing chart (config `home`, default `auto`); the token list lives at `#/market`
test.describe('shell and token list', () => {
  test.use({ appPath: '/#/market' });

  test('lists the registry token with a status badge and never the ticker without its covenant id', async ({ appPage, mock }) => {
    const { id, short } = await seeded(mock);
    await expect(appPage.getByTestId(TESTID.navMarket)).toHaveAttribute('aria-current', 'page');
    const row = appPage.getByTestId(`token-row-${id}`);
    await expect(row).toBeVisible();
    await expect(row.getByTestId(`token-link-${id}`)).toHaveText(`EXKCC (${short}) [unverified, pending review]`);
    // the example registry lists EXKCC as pending review: shown with the badge, but tradable (its template is reviewed; only delisting blocks a token)
    await expect(row.getByTestId('badge-pending-review')).toBeVisible();
    await expect(row).toHaveAttribute('data-tradable', '1');
    // the indexer's open order counts are merged in
    await expect(row).toContainText('11');
    // the full token hash (covenant id) is shown, monospace, never truncated; no market / pair column and no size-convention column
    const hash = row.getByTestId(`token-hash-${id}`);
    await expect(hash).toHaveText(id);
    await expect(hash).toHaveCSS('font-family', /mono|Consolas|Courier/i);
    await expect(appPage.locator('[data-testid="token-list"] thead th')).toHaveText(['Token', 'Token hash (covenant id)', 'Status', 'Asks', 'Bids']);
    await expect(row.locator('[data-testid^="token-pair-"]')).toHaveCount(0);
    await expect(appPage.getByTestId('network-badge')).toHaveText('testnet-10');
    await expect(appPage.getByTestId('footer-statement')).toContainText('non-custodial');
    await expect(appPage.getByTestId('footer-versions')).toContainText('kob-wasm');
  });

  test('search filters, and an unknown route falls back to the list', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await appPage.getByTestId('token-search').fill('zzz');
    await expect(appPage.getByTestId('token-list-empty')).toBeVisible();
    await appPage.getByTestId('token-search').fill('exk');
    await expect(appPage.getByTestId(`token-row-${id}`)).toBeVisible();
    await appPage.goto('/#/definitely-not-a-page');
    await expect(appPage.getByTestId('token-list-view')).toBeVisible();
  });

  test('status bar shows node and indexer health', async ({ appPage }) => {
    await expect(appPage.getByTestId('status-node')).toHaveAttribute('data-level', 'ok');
    await expect(appPage.getByTestId('status-indexer')).toHaveAttribute('data-code', 'ok');
    await expect(appPage.getByTestId('status-indexer')).toContainText('following');
  });

  test('a lookalike of a registered ticker is shown as unverified with a strong warning and cannot be traded', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    const fake = 'bb'.repeat(32);
    await mock.seed({ tokens: [{ ticker: 'EXKCC', covenant_id: fake, extension_commitment: 'cc'.repeat(32), decimals: 8 }] });
    await appPage.reload();
    const row = appPage.getByTestId(`token-row-${fake}`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-source', 'indexer');
    await expect(row).toHaveAttribute('data-tradable', '0');
    await expect(row.getByTestId('lookalike-strong')).toContainText('NOT that token');
    await expect(row.getByTestId('badge-not-in-registry')).toBeVisible();
    await expect(appPage.getByTestId(`token-row-${id}`).getByTestId('lookalike-strong')).toHaveCount(0);
    // the token page repeats the warning and offers no ticket
    await row.getByTestId(`token-link-${fake}`).click();
    await expect(appPage.getByTestId('token-header').getByTestId('lookalike-strong')).toBeVisible();
    await expect(appPage.getByTestId('ticket-unavailable')).toBeVisible();
    await expect(appPage.getByTestId('ticket-container')).toHaveCount(0);
  });

  test('add an unknown token by covenant id: found through the indexer, listed as unverified', async ({ appPage, mock }) => {
    const other = 'dd'.repeat(32);
    await mock.seed({ tokens: [{ ticker: 'NEWT', covenant_id: other, extension_commitment: 'cc'.repeat(32), decimals: 8 }] });
    await appPage.reload();
    // it is already listed by the indexer; paste an id the indexer does not know first
    await appPage.getByTestId('add-token').getByRole('button').first().click();
    await appPage.getByTestId('token-add-input').fill('ee'.repeat(32));
    await appPage.getByTestId('token-add-button').click();
    await expect(appPage.getByTestId('token-add-result')).toContainText('does not know');
    await appPage.getByTestId('token-add-input').fill('nothex');
    await expect(appPage.getByTestId('token-add-button')).toBeDisabled();
    await appPage.getByTestId('token-add-input').fill(other);
    await appPage.getByTestId('token-add-button').click();
    await expect(appPage.getByTestId(`token-row-${other}`)).toHaveAttribute('data-tradable', '0');
  });
});

test.describe('token page', () => {
  test('renders the order book with cumulative depth, and trades from seeded mock data', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await appPage.goto('/#/market');
    await appPage.getByTestId(`token-link-${id}`).click();
    await expect(appPage).toHaveURL(new RegExp(`#/market/${id}$`));
    await expect(appPage.getByTestId('token-title')).toContainText('EXKCC');

    const asks = appPage.getByTestId(TESTID.bookAsks).getByTestId('book-ask-row');
    const bids = appPage.getByTestId(TESTID.bookBids).getByTestId('book-bid-row');
    await expect(asks).toHaveCount(10);
    await expect(bids).toHaveCount(10);
    // asks are displayed with the highest price at the top, the best ask right above the spread; bids best first
    const askPrices = (await asks.evaluateAll((els) => els.map((e) => BigInt((e as HTMLElement).dataset.price!)))) as bigint[];
    const bidPrices = (await bids.evaluateAll((els) => els.map((e) => BigInt((e as HTMLElement).dataset.price!)))) as bigint[];
    expect([...askPrices].sort((a, b) => (a > b ? -1 : 1))).toEqual(askPrices);
    expect([...bidPrices].sort((a, b) => (a > b ? -1 : 1))).toEqual(bidPrices);
    // 2_510_000 sompi per whole token at the touch (0.0251 KAS); bids 2_490_000
    expect(askPrices.at(-1)).toBe(2_510_000n);
    expect(bidPrices[0]).toBe(2_490_000n);
    await expect(appPage.getByTestId('book-mid')).toHaveAttribute('data-value', '2500000');
    await expect(appPage.getByTestId('book-spread')).toContainText('0.0002');
    // fixed decimals per column: every price, size and total of the book has the same number of decimals, right-aligned
    for (const col of [1, 2, 3]) {
      const texts = await appPage.locator('[data-testid="book-asks"] .book-row, [data-testid="book-bids"] .book-row').evaluateAll((els, n) => els.map((e) => (e.children[n] as HTMLElement).textContent ?? ''), col);
      const dec = new Set(texts.map((x) => (x.replace(/^~/, '').includes('.') ? x.replace(/^~/, '').split('.')[1]!.length : 0)));
      expect(texts.length).toBeGreaterThan(2);
      expect(dec.size, `column ${col}: ${texts.join(' | ')}`).toBe(1);
    }
    expect(await asks.last().locator('.book-price').evaluate((e) => getComputedStyle(e).textAlign)).toBe('right');
    // the touch level holds two orders (7 EXKCC at 0.0251)
    await expect(asks.last()).toHaveAttribute('data-amount', '700000000');
    await expect(asks.last()).toContainText('2');
    // the bar of the deepest cumulative level is full width
    await expect(bids.last().locator('.book-bar, i')).toHaveAttribute('style', /width:\s*(100|9\d(\.\d+)?)%/);
    await expect(appPage.getByTestId('depth-chart').locator('svg')).toBeVisible();

    await expect(appPage.getByTestId(TESTID.tradesList).getByTestId('trade-row')).toHaveCount(8);
    // clicking a level prefills the ticket (a stub until the ticket lands): it must not break the page
    await asks.last().click();
    await expect(appPage.getByTestId('token-page')).toBeVisible();
  });

  test('the market page starts with its title bar: no "All tokens" line (the nav bar leads to the list)', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await appPage.goto(`/#/market/${id}`);
    await expect(appPage.getByTestId('token-page')).toBeVisible();
    await expect(appPage.getByTestId('token-back')).toHaveCount(0);
    await expect(appPage.getByTestId('token-page').getByText('All tokens')).toHaveCount(0);
    // the first thing in the page is the title bar
    const page = (await appPage.getByTestId('token-page').boundingBox())!;
    const title = (await appPage.getByTestId('market-pair').boundingBox())!;
    expect(title.y - page.y).toBeLessThan(40);
    await appPage.getByTestId(TESTID.navMarket).click();
    await expect(appPage.getByTestId('token-list-view')).toBeVisible();
  });

  test('shows the header facts, template verification against the pinned hash, and that a pending-review token on a reviewed template is tradable', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await appPage.goto(`/#/market/${id}`);
    await expect(appPage.getByTestId('token-scale')).toContainText('per 100000000 base units');
    await expect(appPage.getByTestId('token-untradable')).toHaveCount(0);
    await expect(appPage.getByTestId('ticket-unavailable')).toHaveCount(0);
    await expect(appPage.getByTestId('ticket-container')).toBeVisible();
    await appPage.getByTestId('template-verification').getByRole('button').first().click();
    await expect(appPage.getByTestId('tpl-registry-hash')).toHaveText(/^[0-9a-f]{64}$/);
    await expect(appPage.getByTestId('tpl-pinned-hash')).toHaveText(await appPage.getByTestId('tpl-registry-hash').innerText());
    await expect(appPage.getByTestId('tpl-verdict')).toContainText('matches');
  });

  test('live indicator: the WebSocket feed connects, and a new fill refreshes the trades without a reload', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await appPage.goto(`/#/market/${id}`);
    await expect(appPage.getByTestId('live-indicator')).toHaveText('Live');
    const trades = appPage.getByTestId(TESTID.tradesList).getByTestId('trade-row');
    await expect(trades).toHaveCount(8);
    const [ask] = await mock.ordersOf('maker', 'active');
    await mock.fill(ask.covenant_id);
    await expect(trades).toHaveCount(9);
  });

  test('falls back to polling when the socket is down, and says so', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await appPage.goto(`/#/market/${id}`);
    await expect(appPage.getByTestId('live-indicator')).toHaveText('Live');
    await mock.closeSockets();
    await expect(appPage.getByTestId('live-indicator')).toContainText('5 s', { timeout: 15_000 });
  });

  test('an unknown covenant id is reported, not blank', async ({ appPage }) => {
    await appPage.goto(`/#/market/${'ab'.repeat(32)}`);
    await expect(appPage.getByTestId('token-not-found')).toBeVisible();
    await appPage.goto('/#/market/xyz');
    await expect(appPage.getByTestId('banner-bad-token')).toBeVisible();
  });
});

test.describe('wallet', () => {
  test('connecting the wallet shows its address and network; disconnect returns to the connect button', async ({ appPage }) => {
    const connect = appPage.getByTestId(TESTID.walletConnectKasware);
    await expect(connect).toBeVisible();
    await expect(appPage.getByTestId(TESTID.walletConnectKaspire)).toHaveCount(0); // only the installed wallet is offered
    await connect.click();
    const addr = appPage.getByTestId(TESTID.walletAddress);
    await expect(addr).toBeVisible();
    await expect(addr).toHaveAttribute('data-value', TEST_KEYS.alice.testnetAddress);
    await expect(addr).toContainText('kaspatest:');
    await expect(addr).not.toHaveText(TEST_KEYS.alice.testnetAddress); // shortened
    await expect(appPage.getByTestId(TESTID.walletNetwork)).toHaveText('testnet-10');
    await expect(appPage.getByTestId('banner-network-mismatch')).toHaveCount(0);
    await appPage.getByTestId('wallet-disconnect').click();
    await expect(appPage.getByTestId(TESTID.walletConnectKasware)).toBeVisible();
  });

  test.describe('wallet on the wrong network', () => {
    // the wallet refuses to switch (KasWare cannot always be switched by a page): the app must block trading and say how to fix it
    test.use({ walletOptions: { network: 'mainnet', allowNetworkSwitch: false } });
    test('blocks trading and explains how to fix it', async ({ appPage }) => {
      await appPage.getByTestId(TESTID.walletConnectKasware).click();
      await expect(appPage.getByTestId(TESTID.walletNetwork)).toHaveText('mainnet');
      const banner = appPage.getByTestId('banner-network-mismatch');
      await expect(banner).toContainText('testnet-10');
      await expect(banner).toContainText('Switch the network in your wallet settings');
      await appPage.goto('/#/orders');
      await expect(appPage.getByTestId('orders-network-blocked')).toBeVisible();
    });
  });

  test.describe('no wallet installed', () => {
    test.use({ walletId: null });
    test('shows install links instead of connect buttons', async ({ appPage }) => {
      await expect(appPage.getByTestId('wallet-install-hint')).toContainText('No Kaspa wallet detected');
      await expect(appPage.getByTestId(TESTID.walletConnectKasware)).toHaveCount(0);
    });
  });
});

test.describe('health banners', () => {
  test.use({ autoOpen: false });

  test('a lagging or stale indexer is announced and the book is marked as possibly out of date', async ({ appPage, mock }) => {
    await mock.setHealth('following', 20_000); // 2000 s behind
    await appPage.goto('/');
    await expect(appPage.getByTestId('banner-indexer')).toContainText('behind');
    await expect(appPage.getByTestId('status-indexer')).toHaveAttribute('data-level', 'bad');
    const { id } = await seeded(mock);
    await appPage.goto(`/#/market/${id}`);
    await expect(appPage.getByTestId('book-stale')).toBeVisible();
    await expect(appPage.getByTestId(TESTID.bookAsks)).toBeVisible(); // degraded, not blank
  });

  test('the lag warning shows only when the indexer is more than 30 s behind (whatever state it reports)', async ({ appPage, mock }) => {
    const { id } = await seeded(mock);
    await mock.setHealth('catching_up', 200); // 20 s behind: nothing to announce
    await appPage.goto(`/#/market/${id}`);
    await expect(appPage.getByTestId('status-indexer')).toHaveAttribute('data-level', 'ok');
    await expect(appPage.getByTestId('banner-indexer')).toHaveCount(0);
    await expect(appPage.getByTestId('book-stale')).toHaveCount(0);
    await mock.setHealth('following', 290); // 29 s
    await appPage.reload();
    await expect(appPage.getByTestId('status-indexer')).toHaveAttribute('data-level', 'ok');
    await expect(appPage.getByTestId('banner-indexer')).toHaveCount(0);
    await mock.setHealth('following', 400); // 40 s: announced
    await appPage.reload();
    await expect(appPage.getByTestId('banner-indexer')).toContainText('behind');
    await expect(appPage.getByTestId('book-stale')).toBeVisible();
  });

  test('an unreachable indexer degrades the page instead of blanking it', async ({ appPage }) => {
    await appPage.route('**/v1/**', (route) => route.abort());
    await appPage.goto('/#/market');
    await expect(appPage.getByTestId('banner-indexer')).toContainText('cannot be reached');
    await expect(appPage.getByTestId('token-list-view')).toBeVisible();
    await expect(appPage.getByTestId('token-list-error')).toBeVisible();
    await expect(appPage.getByTestId('status-indexer')).toHaveAttribute('data-code', 'unreachable');
  });
});
