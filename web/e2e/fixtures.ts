// Playwright fixtures of the mock e2e stack:
//   mock     typed client of the mock server's control API (reset to the default seed before every test)
//   appPage  a page with `window.__KOB_CONFIG__` pointing at the mock indexer / node / registry and a chosen mock wallet installed
//   wallet   the handle of that mock wallet (call log, runtime steering)
// Options (test.use): walletId, walletKey, walletOptions, appConfig, autoOpen, appPath, failOnPageError, covenantChecked.
import { test as base, expect, type Page } from '@playwright/test';
import { startMockServer } from '../mock/server.mjs';
import { TEST_PUBKEYS, TEST_SECRETS } from '../mock/keys.mjs';
import { addressOfSpk, p2pkSpk } from '../mock/address.mjs';
import { installMockWallet, type InstallMockWalletOptions, type MockWalletHandle, type MockWalletId } from './wallet';

export { expect };
export type { MockWalletHandle, MockWalletId };

// ------------------------------------------------------------------------------------------------ deterministic test keys

export type KeyName = keyof typeof TEST_SECRETS;
export interface TestKey { name: KeyName; secretKey: string; pubkey: string; testnetAddress: string; mainnetAddress: string }

/** alice / bob / carol are the users under test, `maker` owns the seeded book. Test money only: the secrets are public. */
export const TEST_KEYS: Record<KeyName, TestKey> = Object.fromEntries(
  (Object.keys(TEST_SECRETS) as KeyName[]).map((name) => [
    name,
    {
      name,
      secretKey: TEST_SECRETS[name],
      pubkey: TEST_PUBKEYS[name],
      testnetAddress: addressOfSpk('kaspatest', p2pkSpk(TEST_PUBKEYS[name]))!,
      mainnetAddress: addressOfSpk('kaspa', p2pkSpk(TEST_PUBKEYS[name]))!,
    },
  ]),
) as Record<KeyName, TestKey>;

// ------------------------------------------------------------------------------------------------ control client

export interface GiveSpec {
  /** KAS (sompi) in `count` UTXOs of this size */
  kas?: bigint | string | number;
  count?: number;
  /** token UTXOs (base units); `token` defaults to the first token of the seed */
  tokens?: { token?: string; amount: bigint | string | number; count?: number; carrier?: bigint | string | number }[];
}

export interface SubmissionSummary {
  txid: string;
  daa: number;
  /** ISO time the mock node accepted the transaction */
  at: string;
  fee: string;
  inputs: number;
  outputs: number;
  /** orders registered by this tx */
  created: { covenantId: string; kind: string; output: number }[];
  /** inputs the transaction spent: outpoint, kind (token | p2pk | order ...) and covenant id */
  spent?: { outpoint: string; kind: string; covenantId: string | null }[];
  /** orders closed by this tx (`entry`: cancel | refund | ...) */
  closed: { covenantId: string; entry: string | null; status: string }[];
  token_outputs: { output: number; owner: string; ownerScheme: number; amount: string; role: string }[];
  /** decoded KOB1 records of the payload */
  records: { type: string; [k: string]: unknown }[];
  /** the accepted transaction (omitted with `submitted(false)`) */
  tx?: any;
}

const jsonReplacer = (_: string, v: unknown) => (typeof v === 'bigint' ? v.toString() : v);

export class MockClient {
  constructor(readonly url: string) {}

  private async call<T = any>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    const res = await fetch(this.url + path, {
      method,
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body, jsonReplacer),
    });
    const json = await res.json().catch(() => null);
    if (!res.ok) throw new Error(`mock ${method} ${path} -> ${res.status}: ${JSON.stringify(json)}`);
    return json as T;
  }
  get = <T = any>(path: string) => this.call<T>('GET', path);
  post = <T = any>(path: string, body: unknown = {}) => this.call<T>('POST', path, body);

  // ---- control
  /** Clears everything and applies the default seed (`seed: false` for an empty chain). */
  reset(opts: { seed?: boolean; fund?: boolean } = {}) {
    return this.post('/mock/reset', opts);
  }
  seedDefaults() {
    return this.reset({ seed: true });
  }
  /** Additive declarative seed: `{tokens, orders, book, fills, utxos}`; see mock/seed.mjs. */
  seed(spec: Record<string, unknown>) {
    return this.post('/mock/seed', spec);
  }
  /** Additive seeded market history (deterministic per `seed`): `{token?, hours = 48, trades = 600, seed = 1, mid?, tick?}`. */
  seedHistory(spec: { token?: string; hours?: number; trades?: number; seed?: number; mid?: number; tick?: number } = {}) {
    return this.post('/mock/seed', { history: spec });
  }
  /** Gives a test key KAS and / or tokens. */
  give(key: KeyName | string, spec: GiveSpec) {
    return this.post('/mock/utxo', { key, ...spec });
  }
  giveKas(key: KeyName | string, sompi: bigint | string | number, count = 1) {
    return this.give(key, { kas: sompi, count });
  }
  giveTokens(key: KeyName | string, amount: bigint | string | number, opts: { token?: string; count?: number; carrier?: bigint | string | number } = {}) {
    return this.give(key, { tokens: [{ amount, ...opts }] });
  }
  async submitted(withTx = true): Promise<SubmissionSummary[]> {
    return (await this.get(`/mock/submitted${withTx ? '' : '?full=0'}`)).items;
  }
  /**
   * Simulates a fill of `amount` base units (default: one whole token, at least the order's minimum fill, at most what is left; price: the
   * current quote); IFD entries also book their exit order.
   */
  fill(covenantId: string, amount?: bigint | string | number, opts: { price?: bigint | string | number; taker?: KeyName | string } = {}) {
    return this.post('/mock/fill', { covenant_id: covenantId, ...(amount === undefined ? {} : { amount }), ...opts });
  }
  arm(covenantId: string) {
    return this.post('/mock/arm', { covenant_id: covenantId });
  }
  /**
   * A token UTXO sent to an order's covenant id from outside the protocol. `token`: a FOREIGN stray of another token (a known ticker / covenant id,
   * or a new covenant id registered as an unlisted token with `program` (default KCC20Ref_8x8) and `ticker`).
   */
  stray(covenantId: string, amount: bigint | string | number, opts: { token?: string; program?: string; ticker?: string; decimals?: number } = {}) {
    return this.post('/mock/stray', { covenant_id: covenantId, amount, ...opts });
  }
  advanceDaa(daa: number) {
    return this.post('/mock/advance-daa', { daa });
  }
  advanceSeconds(seconds: number) {
    return this.post('/mock/advance-daa', { seconds });
  }
  /** The next `count` submissions fail with `message` (HTTP 400 `{error}`). */
  failNextSubmit(message = 'mock: injected submit failure', count = 1) {
    return this.post('/mock/fail-next-submit', { message, count });
  }
  latency(ms: number) {
    return this.post('/mock/latency', { ms });
  }
  /** `following` | `catching_up` | `node_unavailable` | `gap` ... */
  setHealth(state: string, lagDaa = 0) {
    return this.post('/mock/health', { state, lag_daa: lagDaa });
  }
  /** A chain re-org of `blocks` blocks (1 = routine; more than 30 is "deep"); changes nothing in the order data. */
  reorg(blocks = 1) {
    return this.post('/mock/reorg', { blocks });
  }
  /** A re-org that takes the last (partial) simulated fill of an order back out of the indexer's view (see MockChain.revertLastFill). */
  revertFill(covenantId: string, blocks = 1) {
    return this.post('/mock/revert-fill', { covenant_id: covenantId, blocks });
  }
  resync() {
    return this.post('/mock/resync');
  }
  closeSockets() {
    return this.post('/mock/ws-close');
  }
  state() {
    return this.get('/mock/state');
  }
  /** KAS (sompi) and token balances (base units, by ticker) of a key, as strings. */
  balance(key: KeyName | string): Promise<{ pubkey: string; kas: string; tokens: Record<string, string> }> {
    return this.get(`/mock/balance?key=${key}`);
  }

  // ---- indexer reads (same endpoints the app uses)
  async tokens() {
    return (await this.get('/v1/tokens')).tokens as { ticker: string; covenant_id: string; decimals: number; scale: string | number | null }[];
  }
  /** The first token of the seed (the fictional EXKCC of registry/tokens.example.json). */
  async token() {
    return (await this.tokens())[0];
  }
  orders(query = '') {
    return this.get(`/v1/orders${query ? `?${query}` : ''}`).then((r) => r.items as any[]);
  }
  order(covenantId: string) {
    return this.get(`/v1/orders/${covenantId}`);
  }
  async ordersOf(key: KeyName | string, status = 'active') {
    const pub = key in TEST_PUBKEYS ? TEST_PUBKEYS[key as KeyName] : key;
    return this.orders(`maker=${pub}&status=${status}&limit=200`);
  }
  book(token: string, aggregate = true, depth = 20) {
    return this.get(`/v1/books/${token}?aggregate=${aggregate}&depth=${depth}`);
  }

  /** Polls `read` until `pred` holds (for effects the app produces asynchronously). */
  async until<T>(read: () => Promise<T>, pred: (v: T) => boolean, timeoutMs = 10_000): Promise<T> {
    const t0 = Date.now();
    for (;;) {
      const v = await read();
      if (pred(v)) return v;
      if (Date.now() - t0 > timeoutMs) throw new Error(`mock: condition not met within ${timeoutMs} ms (last: ${JSON.stringify(v).slice(0, 300)})`);
      await new Promise((r) => setTimeout(r, 100));
    }
  }
}

// ------------------------------------------------------------------------------------------------ app configuration

/** The `window.__KOB_CONFIG__` the app boots with in e2e runs. */
export function appConfigFor(mockUrl: string, extra: Record<string, unknown> = {}) {
  const { features, ...rest } = extra as { features?: Record<string, unknown> };
  return {
    network: 'testnet-10',
    indexerUrl: mockUrl,
    nodeUrl: mockUrl,
    registryUrl: `${mockUrl}/registry/tokens.json`,
    features: { test: true, kastle: true, ...features },
    ...rest,
  };
}

export interface AppOptions {
  /** the mock wallet installed in `appPage`; null = none (test the no-wallet state) */
  walletId: MockWalletId | null;
  walletKey: KeyName;
  walletOptions: Partial<Omit<InstallMockWalletOptions, 'wallet' | 'secretKey'>>;
  /** merged over the default injected config (network, indexerUrl, nodeUrl, registryUrl, features) */
  appConfig: Record<string, unknown>;
  /** navigate to `appPath` before the test body runs */
  autoOpen: boolean;
  appPath: string;
  /** fail the test on an uncaught page error */
  failOnPageError: boolean;
  /**
   * the one-time covenant-signing check (C5-10, wallet/covenant-probe.ts) of a wallet not declared capable (Kastle) has already passed for every
   * test key (default true: the wallet specs count sign requests); false: the ticket asks for the test signature first
   */
  covenantChecked: boolean;
}

interface TestFixtures {
  mock: MockClient;
  appPage: Page;
  wallet: MockWalletHandle | null;
  /** uncaught page errors + console errors collected while `appPage` is used */
  pageErrors: string[];
}
interface WorkerFixtures {
  mockBase: string;
}

const wallets = new WeakMap<Page, MockWalletHandle>();

export const test = base.extend<AppOptions & TestFixtures, WorkerFixtures>({
  walletId: ['kasware', { option: true }],
  walletKey: ['alice', { option: true }],
  walletOptions: [{}, { option: true }],
  appConfig: [{}, { option: true }],
  autoOpen: [true, { option: true }],
  appPath: ['/', { option: true }],
  failOnPageError: [true, { option: true }],
  covenantChecked: [true, { option: true }],

  // One shared server (started by playwright.config.ts) unless KOB_MOCK_PER_WORKER=1: then every worker owns an in-process server.
  mockBase: [
    async ({}, use) => {
      const shared = process.env.KOB_MOCK_URL;
      if (shared && process.env.KOB_MOCK_PER_WORKER !== '1') {
        await use(shared);
        return;
      }
      const srv = await startMockServer({ port: 0 });
      await use(srv.url);
      await srv.close();
    },
    { scope: 'worker' },
  ],

  mock: async ({ mockBase }, use) => {
    const client = new MockClient(mockBase);
    await client.reset();
    await use(client);
  },

  pageErrors: async ({}, use) => {
    await use([]);
  },

  appPage: async ({ page, mock, walletId, walletKey, walletOptions, appConfig, autoOpen, appPath, failOnPageError, covenantChecked, pageErrors }, use) => {
    page.on('pageerror', (e) => pageErrors.push(`pageerror: ${e.message}`));
    page.on('console', (m) => {
      if (m.type() === 'error') pageErrors.push(`console.error: ${m.text()}`);
    });
    const cfg = appConfigFor(mock.url, appConfig);
    await page.addInitScript((c) => {
      (window as any).__KOB_CONFIG__ = c;
    }, cfg);
    if (covenantChecked && walletId === 'kastle') {
      await page.addInitScript((keys: string[]) => {
        try {
          for (const k of keys) localStorage.setItem(`kob.covenantSign.v1:kastle:${k}`, 'ok');
        } catch {
          /* no storage: the ticket asks for the check */
        }
      }, Object.values(TEST_KEYS).map((k) => k.pubkey));
    }
    if (walletId) wallets.set(page, await installMockWallet(page, { wallet: walletId, secretKey: TEST_KEYS[walletKey].secretKey, ...walletOptions }));
    if (autoOpen) await page.goto(appPath);
    await use(page);
    if (failOnPageError) {
      const fatal = pageErrors.filter((e) => e.startsWith('pageerror:'));
      expect(fatal, 'uncaught page errors').toEqual([]);
    }
  },

  wallet: async ({ appPage }, use) => {
    await use(wallets.get(appPage) ?? null);
  },
});
