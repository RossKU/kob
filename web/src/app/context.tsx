// The Preact contexts every view shares: the services (data layer), the wallet session and the
// per-wallet record store.
import { createContext, type ComponentChildren } from 'preact';
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { LocalStorageRecordStore, MemoryRecordStore, type RecordStore } from '../kob/records';
import { watchWallets } from '../wallet/discover';
import { dispatchTagFrom } from '../wallet/kaspire';
import { createLocalKeyAdapter, type SignerHost } from '../wallet/local-key';
import type { WalletAdapter, WalletId, WalletInfo } from '../wallet/types';
import type { BuiltTx } from '../kob/types';
import { signerRegistryTable, signerTokenHints } from './token-hints';
import { SignerFrame, type SignerMode } from '../ui/shell/SignerFrame';
import { privateKeySigningAllowed } from './mirror';
import type { Services } from './services';
import { watchWallet, type WalletNotice } from './wallet-events';

// --- Services ---

/** The context that carries the app's services. */
export const ServicesContext = createContext<Services | null>(null);

/** The app's services. It throws outside a `ServicesContext.Provider`. */
export function useServices(): Services {
  const s = useContext(ServicesContext);
  if (!s) throw new Error('useServices outside <ServicesContext.Provider>');
  return s;
}

// --- Wallet session ---

/** The wallet session every view reads. */
export interface WalletSession {
  /** The installed wallets. Extensions inject late, so this list grows. */
  detected: WalletAdapter[];
  /**
   * Set once the first look for installed wallets is over, because one was found or
   * DISCOVERY_WAIT_MS passed. Until then "no wallet detected" is not an answer, and Connect waits.
   */
  discovered: boolean;
  adapter: WalletAdapter | null;
  info: WalletInfo | null;
  /** The wallet network differs from the app network, so trading must be blocked. */
  networkMismatch: boolean;
  connecting: boolean;
  error: string | null;
  /**
   * Counts every change of the wallet's key or network after connect. State bound to an older value
   * is stale.
   */
  epoch: number;
  /**
   * The last account or network change the wallet reported. It stays in the bell's current status
   * until dismissed or until the next connect.
   */
  notice: WalletNotice | null;
  dismissNotice(): void;
  connect(id: WalletId): Promise<void>;
  /** Opens the private-key signer frame. The key is typed there, never into this page. */
  openPrivateKey(): void;
  disconnect(): void;
  /** The placement records of the connected key, a memory store when not connected. */
  records: RecordStore;
}

/** The context that carries the wallet session. */
export const WalletContext = createContext<WalletSession | null>(null);

/** The wallet session. It throws outside a `WalletProvider`. */
export function useWallet(): WalletSession {
  const w = useContext(WalletContext);
  if (!w) throw new Error('useWallet outside <WalletProvider>');
  return w;
}

const safeStorage = (): Storage | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/**
 * How long Connect waits in ms for an extension that has not injected yet, before it takes "none"
 * as the answer and offers the private key entry.
 */
export const DISCOVERY_WAIT_MS = 1500;

/** Holds the wallet session and the private-key signer frame for its children. */
export function WalletProvider(props: { services: Services; children: ComponentChildren }) {
  const { config } = props.services;
  const [detected, setDetected] = useState<WalletAdapter[]>([]);
  const [discovered, setDiscovered] = useState(false);
  const [adapter, setAdapter] = useState<WalletAdapter | null>(null);
  const [info, setInfo] = useState<WalletInfo | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);
  const [notice, setNotice] = useState<WalletNotice | null>(null);
  const infoRef = useRef<WalletInfo | null>(null);
  infoRef.current = info;

  // Kaspire signs with `ordered-args` where the plan is just [signature, dispatch tag], else with
  // `wrap-signature`. With `ordered-args` the wallet emits the very script kob-wasm assembles, an
  // independent check of the ABI encoding. Finalize verifies every signature either way. Kaspire
  // 0.5.1 accepts both on TN10.
  const kob = props.services.kob;
  useEffect(() => {
    const stop = watchWallets(
      (d) => {
        setDetected(d);
        if (d.length) setDiscovered(true);
      },
      { kastle: config.features.kastle, timeoutMs: 8000, kaspire: { dispatchTag: dispatchTagFrom(kob) } },
    );
    const h = setTimeout(() => setDiscovered(true), DISCOVERY_WAIT_MS);
    return () => {
      stop();
      clearTimeout(h);
    };
  }, [config.features.kastle, kob]);

  // The private-key signer frame (wallet/local-key.ts, ui/shell/SignerFrame.tsx). It is mounted
  // while its session lives, and removing it forgets the key.
  const [signerMode, setSignerMode] = useState<SignerMode>('closed');
  const hostRef = useRef<SignerHost | null>(null);
  const { sdk, registry } = props.services;
  const localKey = useMemo(
    () =>
      createLocalKeyAdapter({
        host: () => hostRef.current,
        sdk,
        tokenHints: (built: BuiltTx) => signerTokenHints(kob, built, infoRef.current?.pubkey ?? null, registry, config.network),
        registryTable: () => signerRegistryTable(registry),
      }),
    [sdk, registry, kob],
  );

  const disconnect = useCallback(() => {
    infoRef.current = null;
    setAdapter(null);
    setInfo(null);
    setNotice(null);
    hostRef.current?.forget();
    setSignerMode('closed');
  }, []);

  const connect = useCallback(
    async (id: WalletId) => {
      const a = detected.find((d) => d.id === id);
      if (!a) {
        setError(`wallet ${id} not detected`);
        return;
      }
      setSignerMode('closed');
      setConnecting(true);
      setError(null);
      try {
        const i = await a.connect(config.network);
        infoRef.current = i;
        setNotice(null);
        setAdapter(a);
        setInfo(i);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setConnecting(false);
      }
    },
    [detected, config.network],
  );

  const openPrivateKey = useCallback(() => {
    if (!privateKeySigningAllowed()) return; // the GitHub Pages mirror: never offered (app/mirror.ts)
    setError(null);
    setSignerMode('connect');
  }, []);

  const signerEvents = {
    onConnected: () => {
      void localKey.connect(config.network).then(
        (i) => {
          infoRef.current = i;
          setNotice(null);
          setError(null);
          setAdapter(localKey);
          setInfo(i);
          setSignerMode('hidden');
        },
        (e: unknown) => setError(e instanceof Error ? e.message : String(e)),
      );
    },
    onForgotten: () => {
      if (infoRef.current?.id === 'local-key') {
        infoRef.current = null;
        setAdapter(null);
        setInfo(null);
      }
    },
    onRequest: (active: boolean) => setSignerMode((m) => (m === 'closed' ? m : active ? 'sign' : 'hidden')),
  };
  const closeSigner = () => {
    if (signerMode === 'sign') hostRef.current?.cancel();
    else if (signerMode === 'connect') setSignerMode('closed');
  };

  // Account and network changes after connect (wallet-events.ts). Listeners live exactly as long
  // as the session.
  useEffect(() => {
    if (!adapter) return;
    return watchWallet({
      adapter,
      appNetwork: config.network,
      current: () => infoRef.current,
      onChange: (change) => {
        if (change.notice) setNotice(change.notice);
        infoRef.current = change.info;
        if (change.info === null) setAdapter(null);
        setInfo(change.info);
        if (change.identityChanged) setEpoch((n) => n + 1);
      },
    });
  }, [adapter, config.network]);

  const dismissNotice = useCallback(() => setNotice(null), []);

  const records = useMemo<RecordStore>(
    () => (info ? new LocalStorageRecordStore(safeStorage(), config.network, info.pubkey) : new MemoryRecordStore()),
    [info, config.network],
  );

  const session: WalletSession = {
    detected, discovered, adapter, info, connecting, error, epoch, notice, dismissNotice, connect, openPrivateKey, disconnect, records,
    networkMismatch: info !== null && info.network !== config.network,
  };
  return (
    <WalletContext.Provider value={session}>
      {props.children}
      <SignerFrame mode={signerMode} events={signerEvents} onHost={(h) => (hostRef.current = h)} onClose={closeSigner} />
    </WalletContext.Provider>
  );
}
