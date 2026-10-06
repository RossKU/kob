// Account / network change events after connect, for all three adapters against the fake providers of the real event shapes
// (KasWare accountsChanged(string[]) / networkChanged(name); Kaspire also disconnect; Kastle also kas:account_changed).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createKaswareAdapter } from './kasware';
import { createKaspireAdapter } from './kaspire';
import { createKastleAdapter } from './kastle';
import type { WalletAdapter, WalletEvent } from './types';
import { fakeKasware, fakeKaspire, fakeKastle, type FakeControls } from '../testing/fake-wallet-providers';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_PK, MAKER_SK, OTHER_PK, OTHER_SK } from '../testing/token-fixtures';

const sdk = loadKaspaSdkNode();

interface Case {
  name: string;
  make(): { adapter: WalletAdapter; provider: FakeControls; net(name: 'testnet-10' | 'mainnet'): string };
}

const cases: Case[] = [
  {
    name: 'KasWare',
    make: () => {
      const provider = fakeKasware({ sdk, sk: MAKER_SK });
      return { adapter: createKaswareAdapter({ getProvider: () => provider }), provider, net: (n) => (n === 'mainnet' ? 'kaspa_mainnet' : 'kaspa_testnet_10') };
    },
  },
  {
    name: 'Kaspire',
    make: () => {
      const provider = fakeKaspire({ sdk, sk: MAKER_SK, network: 'testnet-10' });
      return { adapter: createKaspireAdapter({ getProvider: () => provider }), provider, net: (n) => n };
    },
  },
  {
    name: 'Kastle',
    make: () => {
      const provider = fakeKastle({ sdk, sk: MAKER_SK, network: 'testnet-10' });
      return { adapter: createKastleAdapter({ getProvider: () => provider }), provider, net: (n) => n };
    },
  },
];

describe.each(cases)('$name: account / network events', ({ make }) => {
  it('reports an account switch and refresh() reads the new key without a connect popup', async () => {
    const { adapter, provider } = make();
    const first = await adapter.connect('testnet-10');
    expect(first.pubkey).toBe(MAKER_PK);
    const seen: WalletEvent[] = [];
    const off = adapter.subscribe!((e) => seen.push(e));
    provider.switchAccount(OTHER_SK);
    expect(seen).toHaveLength(1);
    expect(seen[0]).toMatchObject({ kind: 'accounts' });
    const now = await adapter.refresh!();
    expect(now.pubkey).toBe(OTHER_PK);
    expect(now.address).not.toBe(first.address);
    expect(now.network).toBe('testnet-10');
    off();
  });

  it("reports a network change, and refresh() normalises the wallet's own spelling", async () => {
    const { adapter, provider, net } = make();
    await adapter.connect('testnet-10');
    const seen: WalletEvent[] = [];
    const off = adapter.subscribe!((e) => seen.push(e));
    provider.moveToNetwork(net('mainnet'));
    expect(seen).toEqual([{ kind: 'network', network: net('mainnet') }]);
    expect((await adapter.refresh!()).network).toBe('mainnet');
    provider.moveToNetwork(net('testnet-10'));
    expect((await adapter.refresh!()).network).toBe('testnet-10');
    off();
  });

  it('refresh() rejects with WalletError when the wallet exposes no account any more', async () => {
    const { adapter, provider } = make();
    await adapter.connect('testnet-10');
    const seen: WalletEvent[] = [];
    const off = adapter.subscribe!((e) => seen.push(e));
    provider.lose();
    expect(seen.some((e) => e.kind === 'accounts' && e.accounts.length === 0)).toBe(true);
    await expect(adapter.refresh!()).rejects.toMatchObject({ name: 'WalletError' });
    off();
  });

  it('removes every listener on unsubscribe (idempotent) and delivers nothing afterwards', async () => {
    const { adapter, provider } = make();
    await adapter.connect('testnet-10');
    const before = provider.listenerCount();
    const seen: WalletEvent[] = [];
    const off = adapter.subscribe!((e) => seen.push(e));
    expect(provider.listenerCount()).toBeGreaterThan(before);
    off();
    off();
    expect(provider.listenerCount()).toBe(before);
    provider.switchAccount(OTHER_SK);
    provider.moveToNetwork('mainnet');
    expect(seen).toEqual([]);
  });

  it('two subscriptions are independent: one unsubscribe leaves the other intact', async () => {
    const { adapter, provider } = make();
    await adapter.connect('testnet-10');
    const a: WalletEvent[] = [];
    const b: WalletEvent[] = [];
    const offA = adapter.subscribe!((e) => a.push(e));
    const offB = adapter.subscribe!((e) => b.push(e));
    offA();
    provider.switchAccount(OTHER_SK);
    expect(a).toEqual([]);
    expect(b.length).toBeGreaterThan(0);
    offB();
    expect(provider.listenerCount()).toBe(0);
  });

  it('a provider without an event emitter is polled and unsubscribe stops the timer', async () => {
    vi.useFakeTimers();
    try {
      const { adapter, provider } = make();
      await adapter.connect('testnet-10');
      provider.removeEmitter();
      const seen: WalletEvent[] = [];
      const off = adapter.subscribe!((e) => seen.push(e), { pollMs: 100 });
      await vi.advanceTimersByTimeAsync(250);
      expect(seen).toEqual([]); // the first read is the baseline: nothing to report
      provider.switchAccount(OTHER_SK);
      await vi.advanceTimersByTimeAsync(150);
      expect(seen.filter((e) => e.kind === 'accounts')).toHaveLength(1);
      provider.moveToNetwork('mainnet');
      await vi.advanceTimersByTimeAsync(150);
      expect(seen.filter((e) => e.kind === 'network')).toHaveLength(1);
      off();
      const n = seen.length;
      provider.switchAccount(MAKER_SK);
      await vi.advanceTimersByTimeAsync(500);
      expect(seen).toHaveLength(n);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('poll fallback and hidden tabs', () => {
  const g = globalThis as { document?: unknown };
  const saved = g.document;
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    g.document = saved;
  });

  it('does not read the wallet while the tab is hidden', async () => {
    g.document = { visibilityState: 'hidden' };
    const provider = fakeKasware({ sdk, sk: MAKER_SK });
    const adapter = createKaswareAdapter({ getProvider: () => provider });
    await adapter.connect('testnet-10');
    provider.removeEmitter();
    provider.calls.length = 0;
    const off = adapter.subscribe!(() => undefined, { pollMs: 50 });
    await vi.advanceTimersByTimeAsync(300);
    expect(provider.calls).toEqual([]);
    g.document = { visibilityState: 'visible' };
    await vi.advanceTimersByTimeAsync(100);
    expect(provider.calls.length).toBeGreaterThan(0);
    off();
  });
});

describe('Kaspire echoes its own calls', () => {
  it('requestAccounts and switchNetwork emit events at subscribers: they are triggers only, refresh() decides', async () => {
    const provider = fakeKaspire({ sdk, sk: MAKER_SK, network: 'mainnet' });
    const adapter = createKaspireAdapter({ getProvider: () => provider });
    const seen: WalletEvent[] = [];
    const off = adapter.subscribe!((e) => seen.push(e));
    const info = await adapter.connect('testnet-10'); // requestAccounts, switchNetwork, requestAccounts
    expect(seen.length).toBeGreaterThan(0);
    expect(await adapter.refresh!()).toEqual(info);
    off();
  });
});
