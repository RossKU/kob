// Infrastructure test: the mock server, the injected app config, the mock wallets (all three), kob-wasm and the official SDK working
// together in a real browser, driven through the placeholder page (e2e/fixtures-site). The per-order-type specs replace the placeholder
// with the real UI later; this spec proves the fixtures they rely on.
import { expect, test, TEST_KEYS, type KeyName, type MockClient } from '../fixtures';
import { TESTID } from '../testids';
import type { ActionRequest, KeyUtxo, TokenUtxo } from '../../src/kob/types';

const CARRIER = '1000000000';
const TEN_KAS_X5 = 5n * 10n ** 10n;
const TOKENS_10 = 10n * 10n ** 8n; // 10 whole tokens of the seeded token (scale 1e8 base units)

/**
 * A createOrder request for an ask of `wholeTokens` whole tokens by `key` at `price` sompi per whole token, assembled from the mock indexer + node like
 * the app does. The order state is modelled on a seeded book order (same token identity, scale and minimum fill) with the maker, price and amount
 * replaced.
 */
async function askRequest(mock: MockClient, key: KeyName, wholeTokens: number, price: number): Promise<ActionRequest> {
  const tok = await mock.token();
  const k = TEST_KEYS[key];
  const utxos = (await mock.post('/node/utxos', { addresses: [k.testnetAddress] })).entries as any[];
  const funding: KeyUtxo[] = utxos.map((e) => ({ transactionId: e.transactionId, index: e.index, amount: e.amount, blockDaaScore: e.blockDaaScore, covenantId: null, pubkey: k.pubkey }));
  const tokens = (await mock.get(`/v1/token-utxos?owner=${k.pubkey}&token=${tok.covenant_id}&spent=false`)).items.map(
    (t: any): TokenUtxo => ({ transactionId: t.txid, index: t.index, amount: t.value, blockDaaScore: String(t.created_daa), covenantId: t.token, state: t.state }),
  );
  const [seeded] = (await mock.book(tok.covenant_id, false, 1)).asks;
  const view = await mock.order(seeded.covenant_id);
  const order = { kind: 'KobAsk', state: { ...view.state.state, maker: k.pubkey, price: String(price), amountLeft: String(BigInt(wholeTokens) * 10n ** 8n) } };
  return { action: 'createOrder', order: order as any, value: CARRIER, tokens, tokenCarrier: CARRIER, funding, change: null, lockTime: '0', deadline: null, records: [], fee: { feeRate: null } };
}

const WALLETS = ['kasware', 'kaspire', 'kastle'] as const;
const signAndSubmit = (page: import('@playwright/test').Page, request: ActionRequest) => page.evaluate((r) => (window as any).__infra.signAndSubmit(r), request);
const signAndSubmitError = (page: import('@playwright/test').Page, request: ActionRequest) =>
  page.evaluate((r) => (window as any).__infra.signAndSubmit(r).then(() => null, (e: Error) => e.message), request);

test.describe('fixtures: mock server + injected config', () => {
  test.use({ walletId: null });

  test('the page boots with the injected config and exposes the official SDK for the mock wallets', async ({ appPage }) => {
    await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('ready', { timeout: 30_000 });
    const cfg = await appPage.evaluate(() => (window as any).__KOB_CONFIG__);
    expect(cfg).toMatchObject({ network: 'testnet-10', features: { test: true, kastle: true } });
    expect(cfg.indexerUrl).toBe(cfg.nodeUrl);
    expect(await appPage.evaluate(() => typeof (window as any).__kobKaspa?.createInputSignature)).toBe('function');
    // the registry the config points at is served by the mock
    const reg = await appPage.evaluate(async (u) => (await fetch(u)).json(), cfg.registryUrl);
    expect(reg.tokens[0].ticker).toBe('EXKCC');
    expect(await appPage.evaluate(() => (window as any).kasware ?? null)).toBeNull(); // walletId: null installs nothing
  });

  test('mock client: default seed, funding keys, balances, latency, health, reset', async ({ mock }) => {
    const tok = await mock.token();
    expect(tok.ticker).toBe('EXKCC');
    expect((await mock.book(tok.covenant_id)).asks).toHaveLength(10);
    await mock.give('alice', { kas: 10n ** 10n, count: 2, tokens: [{ amount: 5n * 10n ** 8n }] });
    expect(await mock.balance('alice')).toMatchObject({ kas: String(2n * 10n ** 10n), tokens: { EXKCC: String(5n * 10n ** 8n) } });
    await mock.setHealth('catching_up', 300);
    expect((await mock.get('/v1/health')).lag_seconds).toBe(30);
    await mock.latency(150);
    const t0 = Date.now();
    await mock.get('/v1/health');
    expect(Date.now() - t0).toBeGreaterThanOrEqual(140);
    await mock.reset();
    expect((await mock.balance('alice')).kas).toBe('0');
    expect((await mock.state()).latency_ms).toBe(0);
  });

  test('mock client: simulated fills and the fill history', async ({ mock }) => {
    const tok = await mock.token();
    const [best] = (await mock.book(tok.covenant_id, false, 1)).asks;
    const r = await mock.fill(best.covenant_id, 10n ** 8n, { taker: 'bob' });
    expect(r).toMatchObject({ covenant_id: best.covenant_id, status: 'partial', filled_amount: String(10n ** 8n) });
    expect((await mock.balance('bob')).tokens.EXKCC).toBe(String(10n ** 8n));
    expect((await mock.get('/v1/fills?limit=1')).items[0]).toMatchObject({ covenant_id: best.covenant_id, amount: String(10n ** 8n) });
  });
});

for (const walletId of WALLETS) {
  test.describe(`${walletId}: connect, sign, submit`, () => {
    test.use({ walletId });

    test('connects (network labels normalised) and signs a create-ask that the mock node accepts', async ({ appPage, mock, wallet }) => {
      await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('ready', { timeout: 30_000 });
      await appPage.getByTestId(`wallet-connect-${walletId}`).click();
      await expect(appPage.getByTestId(TESTID.walletAddress)).toHaveText(TEST_KEYS.alice.testnetAddress);
      await expect(appPage.getByTestId(TESTID.walletNetwork)).toHaveText('testnet-10');

      const tok = await mock.token();
      await mock.giveKas('alice', TEN_KAS_X5);
      await mock.giveTokens('alice', TOKENS_10);
      const request = await askRequest(mock, 'alice', 3, 2_505_000);
      const before = (await mock.book(tok.covenant_id)).asks.length;

      const result = await signAndSubmit(appPage, request);
      await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('accepted');
      await expect(appPage.getByTestId(TESTID.txId)).toHaveText(result.txid);

      // what the wallet was asked to sign
      const calls = await wallet!.calls();
      expect(calls).toHaveLength(1);
      expect(calls[0]).toMatchObject({ method: walletId === 'kastle' ? 'signTx' : 'signPskt', status: 'signed' });
      expect(calls[0].inputs.length).toBeGreaterThanOrEqual(2);
      expect(calls[0].tx.version).toBe(1);
      expect(calls[0].signedInputs.length).toBe(calls[0].inputs.length);
      if (walletId === 'kastle') expect(calls[0].scripts).toEqual([{ inputIndex: 0, scriptHex: '', signType: 'All' }]);
      if (walletId === 'kaspire') expect(calls[0].scripts![0]).toMatchObject({ inputIndex: 0, signatureScript: { mode: 'wrap-signature' } });

      // the order exists, with exact custody, in the book and in the maker's list
      const cov = result.covenants[0].covenantId;
      const order = await mock.until(() => mock.order(cov), (o) => o.status === 'open');
      expect(order).toMatchObject({ contract: 'KobAsk', maker: TEST_KEYS.alice.pubkey, amount_left: String(3n * 10n ** 8n), custody: { ok: true } });
      expect((await mock.book(tok.covenant_id)).asks.length).toBe(before + 1);
      const sub = (await mock.submitted(false)).at(-1)!;
      expect(sub.txid).toBe(result.txid);
      expect(sub.created).toEqual([{ covenantId: cov, kind: 'KobAsk', output: 0 }]);
      expect((await mock.ordersOf('alice')).map((o) => o.covenant_id)).toEqual([cov]);
    });

    test('a rejecting wallet surfaces the wallet error and leaves the chain untouched', async ({ appPage, mock, wallet }) => {
      await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('ready', { timeout: 30_000 });
      await appPage.getByTestId(`wallet-connect-${walletId}`).click();
      await expect(appPage.getByTestId(TESTID.walletAddress)).not.toBeEmpty();
      await mock.giveKas('alice', TEN_KAS_X5);
      await mock.giveTokens('alice', TOKENS_10);
      const request = await askRequest(mock, 'alice', 2, 2_505_000);
      await wallet!.configure({ approve: false });
      expect(await signAndSubmitError(appPage, request)).toMatch(/reject/i);
      expect((await wallet!.lastCall())!.status).toBe('rejected');
      expect(await mock.submitted(false)).toEqual([]);
      // approving afterwards works; the failed attempt consumed nothing
      await wallet!.configure({ approve: true });
      await signAndSubmit(appPage, request);
      expect(await mock.submitted(false)).toHaveLength(1);
    });
  });
}

test.describe('failure injection', () => {
  test.use({ walletId: 'kasware' });

  test('a submit failure comes back as the node error text; a wrong signature never reaches the node', async ({ appPage, mock, wallet }) => {
    await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('ready', { timeout: 30_000 });
    await appPage.getByTestId(TESTID.walletConnectKasware).click();
    await expect(appPage.getByTestId(TESTID.walletAddress)).not.toBeEmpty();
    await mock.giveKas('alice', TEN_KAS_X5);
    await mock.giveTokens('alice', TOKENS_10);
    const request = await askRequest(mock, 'alice', 2, 2_505_000);
    await mock.failNextSubmit('mempool is full');
    expect(await signAndSubmitError(appPage, request)).toBe('mempool is full');
    await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('rejected: mempool is full');
    // the wallet returns a signature over the wrong digest: kob-wasm's finalize refuses it before anything is submitted
    await wallet!.configure({ wrongSignature: true });
    expect(await signAndSubmitError(appPage, request)).toMatch(/signature/i);
    expect(await mock.submitted(false)).toEqual([]);
  });
});

test.describe('late injection', () => {
  test.use({ walletId: 'kaspire', walletOptions: { injectDelayMs: 3000 } });

  test('the provider is absent at page start and found by polling', async ({ appPage }) => {
    expect(await appPage.evaluate(() => typeof (window as any).kaspire)).toBe('undefined');
    expect(await appPage.evaluate(() => typeof (window as any).__mockWallet)).toBe('object'); // the test handle exists from the start
    await expect(appPage.getByTestId(TESTID.txStatus)).toHaveText('ready', { timeout: 30_000 });
    // the placeholder polls for the provider like the real adapters do
    await appPage.getByTestId(TESTID.walletConnectKaspire).click();
    await expect(appPage.getByTestId(TESTID.walletAddress)).toHaveText(TEST_KEYS.alice.testnetAddress);
  });
});
