// Playwright helper: injects a mock wallet (src/testing/mock-wallets.ts) into a page BEFORE any page script runs.
// Call `installMockWallet(page, {...})` before `page.goto`. The wallet signs in the page with `window.__kobKaspa` (the app exposes the
// official SDK there when its injected config has `features.test`), exactly like the real extension signs with its own wasm.
import type { Page } from '@playwright/test';
import { DEFAULT_WALLET_CONFIG, WALLET_DEFAULTS, mockWalletInitScript, type MockWalletCall, type MockWalletConfig, type MockWalletId } from '../src/testing/mock-wallets';
import { pubkeyOf } from '../src/testing/local-signer';
import { addressOfSpk, p2pkSpk } from '../mock/address.mjs';

export type { MockWalletCall, MockWalletConfig, MockWalletId };

export interface InstallMockWalletOptions extends Partial<Omit<MockWalletConfig, 'wallet' | 'secretKey' | 'pubkey' | 'addresses'>> {
  wallet: MockWalletId;
  /** secret key (64 hex) the wallet signs with; see `TEST_KEYS` in fixtures.ts */
  secretKey: string;
}

/** Full page-side config: public key and per-network addresses are derived from the secret key here (node side). */
export function buildWalletConfig(o: InstallMockWalletOptions): MockWalletConfig {
  const { wallet, secretKey, ...rest } = o;
  const pubkey = pubkeyOf(secretKey);
  const spk = p2pkSpk(pubkey);
  return {
    ...DEFAULT_WALLET_CONFIG,
    ...WALLET_DEFAULTS[wallet],
    ...stripUndefined(rest),
    wallet,
    secretKey,
    pubkey,
    addresses: { 'testnet-10': addressOfSpk('kaspatest', spk)!, mainnet: addressOfSpk('kaspa', spk)! },
  } as MockWalletConfig;
}

const stripUndefined = <T extends object>(o: T): Partial<T> => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as Partial<T>;

/** A handle to steer the wallet of one page at runtime and to inspect what it was asked to sign. */
export interface MockWalletHandle {
  config: MockWalletConfig;
  /** every sign request the wallet received (deep copies, safe to assert on) */
  calls(): Promise<MockWalletCall[]>;
  lastCall(): Promise<MockWalletCall | null>;
  /** resolves once the wallet has received at least `n` sign requests (returns them) */
  waitForCalls(n?: number, timeoutMs?: number): Promise<MockWalletCall[]>;
  /** changes behaviour of the LIVE page (approve / reject / delay / wrongSignature / ...). A reload restores the installed options. */
  configure(patch: Partial<MockWalletConfig>): Promise<void>;
  clearCalls(): Promise<void>;
  /** fires a wallet event on the live page (`accountsChanged`, `networkChanged`) */
  emit(event: string, ...args: unknown[]): Promise<void>;
  /** the wallet's network changes (address prefix follows) and `networkChanged` fires */
  setNetwork(network: 'testnet-10' | 'mainnet'): Promise<void>;
  /** the user picks another account in the wallet (another key) and `accountsChanged` fires */
  setAccount(secretKey: string): Promise<void>;
  /** the wallet locks / the site loses its connection: no account is exposed, `accountsChanged([])` fires */
  lose(): Promise<void>;
  /** provider listeners the page has registered (leak checks) */
  listenerCount(): Promise<number>;
}

export async function installMockWallet(page: Page, opts: InstallMockWalletOptions): Promise<MockWalletHandle> {
  const config = buildWalletConfig(opts);
  await page.addInitScript({ content: mockWalletInitScript(config) });
  const handle: MockWalletHandle = {
    config,
    calls: () => page.evaluate(() => JSON.parse(JSON.stringify((window as any).__mockWallet.calls))),
    lastCall: () =>
      page.evaluate(() => {
        const c = (window as any).__mockWallet.lastCall();
        return c ? JSON.parse(JSON.stringify(c)) : null;
      }),
    async waitForCalls(n = 1, timeoutMs = 15_000) {
      const t0 = Date.now();
      for (;;) {
        const calls = await handle.calls();
        if (calls.length >= n) return calls;
        if (Date.now() - t0 > timeoutMs) throw new Error(`mock wallet: expected ${n} sign request(s), got ${calls.length} after ${timeoutMs} ms`);
        await new Promise((r) => setTimeout(r, 50));
      }
    },
    configure: (patch) => page.evaluate((p) => void (window as any).__mockWallet.configure(p), patch),
    clearCalls: () => page.evaluate(() => void (window as any).__mockWallet.clearCalls()),
    emit: (event, ...args) => page.evaluate((a) => void (window as any).__mockWallet.emit(a.event, ...a.args), { event, args }),
    setNetwork: (network) => page.evaluate((n) => void (window as any).__mockWallet.setNetwork(n), network),
    setAccount: (secretKey) => {
      const pubkey = pubkeyOf(secretKey);
      const spk = p2pkSpk(pubkey);
      const a = { secretKey, pubkey, addresses: { 'testnet-10': addressOfSpk('kaspatest', spk)!, mainnet: addressOfSpk('kaspa', spk)! } };
      return page.evaluate((acc) => void (window as any).__mockWallet.setAccount(acc), a);
    },
    lose: () => page.evaluate(() => void (window as any).__mockWallet.lose()),
    listenerCount: () => page.evaluate(() => (window as any).__mockWallet.listenerCount() as number),
  };
  return handle;
}
