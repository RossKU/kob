// `--mock` mode of the real-wallet run: the SAME scenarios, selectors and checks, but against the offline mock stack of web/e2e (mock/server.mjs:
// mock indexer + mock node that runs every submitted transaction through kob-wasm) with the in-page mock wallets of src/testing/mock-wallets.ts
// instead of the released extensions and a TN10 node. It proves that every step of the TN10 run still matches the UI and the protocol; it
// cannot prove anything about the real extensions, their popups or the real node (README "What the mock mode does not prove").
// Nothing runs on import.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { WEB_ROOT, sleep } from './common.mjs';

/** Deterministic test keys of the mock (mock/keys.mjs): one per wallet, `maker` is the counterparty of the market scenario (the DEV key on TN10). */
export const MOCK_KEY_OF = { kasware: 'alice', kaspire: 'bob', kastle: 'carol', dev: 'maker' };

let walletModule = null;
/**
 * src/testing/mock-wallets.ts as a module (it has no imports): transpiled with vite's oxc transform, so this works on any Node 22 without
 * TypeScript support. `mockWalletInitScript(config)` is the page script the Playwright fixtures inject (e2e/wallet.ts).
 */
export async function mockWallets() {
  if (walletModule) return walletModule;
  const { transformWithOxc } = await import('vite');
  const src = readFileSync(join(WEB_ROOT, 'src', 'testing', 'mock-wallets.ts'), 'utf8');
  const { code } = await transformWithOxc(src, 'mock-wallets.ts', { lang: 'ts' });
  walletModule = await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`);
  return walletModule;
}

/** The page config of a mock wallet for `secretKey` (e2e/wallet.ts `buildWalletConfig`, without the TypeScript imports). */
export async function mockWalletConfig(wallet, secretKey) {
  const { DEFAULT_WALLET_CONFIG, WALLET_DEFAULTS } = await mockWallets();
  const { pubkeyOfSecret } = await import('../mock/keys.mjs');
  const { addressOfSpk, p2pkSpk } = await import('../mock/address.mjs');
  const pubkey = pubkeyOfSecret(secretKey);
  const spk = p2pkSpk(pubkey);
  return {
    ...DEFAULT_WALLET_CONFIG,
    ...WALLET_DEFAULTS[wallet],
    wallet,
    secretKey,
    pubkey,
    addresses: { 'testnet-10': addressOfSpk('kaspatest', spk), mainnet: addressOfSpk('kaspa', spk) },
  };
}

/** Injects the mock wallet into every page of `ctx` (before any page script). Returns the wallet config (address, pubkey). */
export async function installMockWallet(ctx, wallet, secretKey) {
  const config = await mockWalletConfig(wallet, secretKey);
  const { mockWalletInitScript } = await mockWallets();
  await ctx.addInitScript({ content: mockWalletInitScript(config) });
  return config;
}

/** Starts the mock stack in this process on a free port with an EMPTY chain (no seeded token, book or funds). */
export async function startMock() {
  const { startMockServer } = await import('../mock/server.mjs');
  const { TEST_SECRETS, TEST_PUBKEYS } = await import('../mock/keys.mjs');
  const srv = await startMockServer({ port: 0, seed: false });
  const call = async (method, path, body) => {
    const res = await fetch(srv.url + path, { method, headers: body === undefined ? undefined : { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
    const json = await res.json().catch(() => null);
    if (!res.ok) throw new Error(`mock ${method} ${path} -> ${res.status}: ${JSON.stringify(json)}`);
    return json;
  };
  return {
    url: srv.url,
    secrets: TEST_SECRETS,
    pubkeys: TEST_PUBKEYS,
    get: (p) => call('GET', p),
    post: (p, b = {}) => call('POST', p, b),
    /** gives `key` (test key name or x-only pubkey) `kas` KAS in `count` UTXOs */
    giveKas: (key, kas, count = 4) => call('POST', '/mock/utxo', { key, kas: String(BigInt(Math.round(kas * 1e8))), count }),
    /** registers an issued token with the mock indexer (the real indexer learns it from its registry): books, token-utxos and orders of it */
    registerToken: (t) => call('POST', '/mock/seed', { tokens: [t] }),
    close: () => srv.close(),
  };
}

/**
 * The independent node checker of the mock: the same interface as driver.mjs `NodeChecker` (TN10, official SDK), read from the mock node's
 * UTXO set over HTTP (`POST /node/utxos`). Addresses are derived with mock/address.mjs.
 */
export class MockNodeChecker {
  constructor(mockUrl) {
    this.url = mockUrl;
    this.addr = null;
  }

  async #a() {
    return (this.addr ??= await import('../mock/address.mjs'));
  }

  async ping() {
    const r = await fetch(`${this.url}/node/info`);
    if (!r.ok) throw new Error(`mock node: HTTP ${r.status}`);
    return (await r.json()).virtualDaaScore ?? 0;
  }

  async utxos(address) {
    const r = await fetch(`${this.url}/node/utxos`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ addresses: [address] }) });
    if (!r.ok) throw new Error(`mock node utxos: HTTP ${r.status}`);
    return (await r.json()).entries.map((e) => ({ txid: e.transactionId, index: e.index, amount: BigInt(e.amount), covenantId: e.covenantId ?? null }));
  }

  /** synchronous like the SDK one: the address module is loaded by `init()` */
  addressOfSpk(spk) {
    const s = typeof spk === 'string' ? spk : `${Number(spk.version ?? 0).toString(16).padStart(4, '0')}${spk.script ?? spk.scriptPublicKey}`;
    return this.addr.addressOfSpk('kaspatest', s);
  }

  async init() {
    await this.#a();
    return this;
  }

  async isUnspent(address, txid, index) {
    return (await this.utxos(address)).some((u) => u.txid === txid && u.index === index);
  }

  async waitOutpoint(address, txid, index, want, timeoutMs = 30_000) {
    const t0 = Date.now();
    for (;;) {
      let now = null;
      try {
        now = await this.isUnspent(address, txid, index);
      } catch {
        /* keep polling */
      }
      if (now === want) return { ok: true, ms: Date.now() - t0 };
      if (Date.now() - t0 > timeoutMs) return { ok: false, ms: Date.now() - t0 };
      await sleep(500);
    }
  }
}
