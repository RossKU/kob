// The app's reaction to wallet changes after connect: the pure reducer and the watcher that the WalletProvider runs.
import { describe, expect, it, vi } from 'vitest';
import { reactToWalletChange, sameWallet, watchWallet, type WalletChange } from './wallet-events';
import { createKaswareAdapter } from '../wallet/kasware';
import { createKaspireAdapter } from '../wallet/kaspire';
import { createKastleAdapter } from '../wallet/kastle';
import { WalletError, type WalletAdapter, type WalletInfo } from '../wallet/types';
import { fakeKasware, fakeKaspire, fakeKastle, type FakeControls } from '../testing/fake-wallet-providers';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_PK, MAKER_SK, OTHER_PK, OTHER_SK } from '../testing/token-fixtures';

const sdk = loadKaspaSdkNode();
const APP = 'testnet-10';

const info = (o: Partial<WalletInfo> = {}): WalletInfo => ({ id: 'kasware', label: 'KasWare', address: 'kaspatest:a', pubkey: MAKER_PK, network: APP, version: '1', ...o });

describe('reactToWalletChange', () => {
  it('an unchanged wallet (or an echo with another version string) is not a change', () => {
    const prev = info();
    const r = reactToWalletChange(prev, info({ version: '2' }), APP);
    expect(r).toEqual({ info: prev, notice: null, identityChanged: false });
    expect(sameWallet(prev, info({ version: '3' }))).toBe(true);
  });

  it('another key is an account change and marks the identity as changed', () => {
    const next = info({ pubkey: OTHER_PK, address: 'kaspatest:b' });
    const r = reactToWalletChange(info(), next, APP);
    expect(r.info).toBe(next);
    expect(r.identityChanged).toBe(true);
    expect(r.notice).toMatchObject({ kind: 'account-changed', to: { address: 'kaspatest:b' }, from: { address: 'kaspatest:a' } });
  });

  it('a network change is reported, and coming back to the app network is a restore', () => {
    const away = reactToWalletChange(info(), info({ network: 'mainnet' }), APP);
    expect(away.notice).toMatchObject({ kind: 'network-changed', to: { network: 'mainnet' } });
    expect(away.identityChanged).toBe(true);
    const back = reactToWalletChange(away.info!, info(), APP);
    expect(back.notice).toMatchObject({ kind: 'network-restored' });
    // between two foreign networks it stays a plain change
    expect(reactToWalletChange(info({ network: 'mainnet' }), info({ network: 'testnet-11' }), APP).notice?.kind).toBe('network-changed');
  });

  it('a wallet that reports no account closes the session', () => {
    const r = reactToWalletChange(info(), null, APP);
    expect(r.info).toBeNull();
    expect(r.notice).toMatchObject({ kind: 'wallet-lost', wallet: 'KasWare' });
  });

  it('the same key and network under a re-encoded address updates the string without a notice', () => {
    const r = reactToWalletChange(info(), info({ address: 'kaspatest:a-recoded' }), APP);
    expect(r.notice).toBeNull();
    expect(r.identityChanged).toBe(false);
    expect(r.info!.address).toBe('kaspatest:a-recoded');
  });
});

interface Rig {
  name: string;
  adapter: WalletAdapter;
  provider: FakeControls;
  net(n: 'testnet-10' | 'mainnet'): string;
}
const rigs: (() => Rig)[] = [
  () => {
    const provider = fakeKasware({ sdk, sk: MAKER_SK });
    return { name: 'KasWare', adapter: createKaswareAdapter({ getProvider: () => provider }), provider, net: (n) => (n === 'mainnet' ? 'kaspa_mainnet' : 'kaspa_testnet_10') };
  },
  () => {
    const provider = fakeKaspire({ sdk, sk: MAKER_SK, network: 'testnet-10' });
    return { name: 'Kaspire', adapter: createKaspireAdapter({ getProvider: () => provider }), provider, net: (n) => n };
  },
  () => {
    const provider = fakeKastle({ sdk, sk: MAKER_SK, network: 'testnet-10' });
    return { name: 'Kastle', adapter: createKastleAdapter({ getProvider: () => provider }), provider, net: (n) => n };
  },
];

const flush = async () => {
  for (let i = 0; i < 20; i++) await new Promise<void>((r) => setTimeout(r, 0));
};

describe.each(rigs)('watchWallet with %#', (make) => {
  async function start() {
    const rig = make();
    let current: WalletInfo | null = await rig.adapter.connect(APP);
    const changes: WalletChange[] = [];
    const stop = watchWallet({
      adapter: rig.adapter,
      appNetwork: APP,
      current: () => current,
      onChange: (c) => {
        changes.push(c);
        current = c.info;
      },
    });
    return { rig, changes, stop, now: () => current };
  }

  it('account switch: the session moves to the new key, once, with a notice', async () => {
    const { rig, changes, stop, now } = await start();
    rig.provider.switchAccount(OTHER_SK);
    await flush();
    expect(changes).toHaveLength(1);
    expect(changes[0]!.notice?.kind).toBe('account-changed');
    expect(now()!.pubkey).toBe(OTHER_PK);
    stop();
  });

  it('network switch away and back: mismatch, then restore, and the same wallet stays the same', async () => {
    const { rig, changes, stop, now } = await start();
    rig.provider.moveToNetwork(rig.net('mainnet'));
    await flush();
    expect(now()!.network).toBe('mainnet');
    expect(changes.map((c) => c.notice?.kind)).toEqual(['network-changed']);
    rig.provider.moveToNetwork(rig.net('testnet-10'));
    await flush();
    expect(now()!.network).toBe(APP);
    expect(changes.map((c) => c.notice?.kind)).toEqual(['network-changed', 'network-restored']);
    expect(now()!.pubkey).toBe(MAKER_PK);
    stop();
  });

  it('a burst of events and echoes of an unchanged wallet produce no change at all', async () => {
    const { rig, changes, stop } = await start();
    for (let i = 0; i < 5; i++) rig.provider.emit('accountsChanged', ['whatever']);
    rig.provider.emit('networkChanged', rig.net('testnet-10'));
    await flush();
    expect(changes).toEqual([]);
    stop();
  });

  it('a burst around a real switch settles on the final state, reported once', async () => {
    const { rig, changes, stop, now } = await start();
    rig.provider.switchAccount(OTHER_SK);
    rig.provider.emit('accountsChanged', ['x']);
    rig.provider.emit('accountsChanged', ['y']);
    await flush();
    expect(changes).toHaveLength(1);
    expect(now()!.pubkey).toBe(OTHER_PK);
    stop();
  });

  it('an account that disappears closes the session with a wallet-lost notice', async () => {
    const { rig, changes, stop, now } = await start();
    rig.provider.lose();
    await flush();
    expect(now()).toBeNull();
    expect(changes.at(-1)!.notice?.kind).toBe('wallet-lost');
    stop();
  });

  it('stop() removes every listener: nothing fires afterwards, no leak', async () => {
    const { rig, changes, stop } = await start();
    const during = rig.provider.listenerCount();
    expect(during).toBeGreaterThan(0);
    stop();
    stop();
    expect(rig.provider.listenerCount()).toBe(0);
    rig.provider.switchAccount(OTHER_SK);
    await flush();
    expect(changes).toEqual([]);
  });

  it('an event that arrives after stop() while a re-read is in flight is dropped', async () => {
    const { rig, changes, stop } = await start();
    rig.provider.switchAccount(OTHER_SK); // starts the re-read
    stop();
    await flush();
    expect(changes).toEqual([]);
  });
});

describe('watchWallet details', () => {
  it('an adapter without subscribe / refresh is left alone', () => {
    const adapter = { id: 'kasware', label: 'x', detect: () => true } as unknown as WalletAdapter;
    const stop = watchWallet({ adapter, appNetwork: APP, current: () => info(), onChange: () => { throw new Error('unexpected'); } });
    expect(() => stop()).not.toThrow();
  });

  it('a timed-out re-read is retried once, a second failure closes the session', async () => {
    vi.useFakeTimers();
    try {
      let handler: (() => void) | null = null;
      let calls = 0;
      const adapter = {
        id: 'kasware',
        label: 'KasWare',
        detect: () => true,
        subscribe: (h: () => void) => {
          handler = h;
          return () => undefined;
        },
        refresh: async () => {
          calls++;
          if (calls === 1) throw new WalletError('timeout', 'slow');
          return info({ pubkey: OTHER_PK });
        },
      } as unknown as WalletAdapter;
      const changes: WalletChange[] = [];
      watchWallet({ adapter, appNetwork: APP, current: () => info(), onChange: (c) => changes.push(c), retryMs: 10 });
      handler!();
      await vi.advanceTimersByTimeAsync(50);
      expect(calls).toBe(2);
      expect(changes[0]!.notice?.kind).toBe('account-changed');

      calls = 0;
      const failing = { ...adapter, refresh: async () => { throw new WalletError('timeout', 'slow'); } } as unknown as WalletAdapter;
      const c2: WalletChange[] = [];
      watchWallet({ adapter: failing, appNetwork: APP, current: () => info(), onChange: (c) => c2.push(c), retryMs: 10 });
      handler!();
      await vi.advanceTimersByTimeAsync(50);
      expect(c2[0]!.notice?.kind).toBe('wallet-lost');
    } finally {
      vi.useRealTimers();
    }
  });
});
