// Preact contexts shared by every view: services (data layer), wallet session, per-wallet record store.
import { createContext, type ComponentChildren } from 'preact';
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { LocalStorageRecordStore, MemoryRecordStore, type RecordStore } from '../kob/records';
import { watchWallets } from '../wallet/discover';
import { dispatchTagFrom } from '../wallet/kaspire';
import { createLocalKeyAdapter, type SignerHost } from '../wallet/local-key';
import type { WalletAdapter, WalletId, WalletInfo } from '../wallet/types';
import type { BuiltTx } from '../kob/types';
import { signerTokenHints } from './token-hints';
import { SignerFrame, type SignerMode } from '../ui/shell/SignerFrame';
import type { Services } from './services';
import { watchWallet, type WalletNotice } from './wallet-events';

// ------------------------------------------------------------------------------------------------ services

export const ServicesContext = createContext<Services | null>(null);

export function useServices(): Services {
  const s = useContext(ServicesContext);
  if (!s) throw new Error('useServices outside <ServicesContext.Provider>');
  return s;
}

// ------------------------------------------------------------------------------------------------ wallet session

export interface WalletSession {
  /** installed wallets (extensions inject late: this list grows) */
  detected: WalletAdapter[];
  adapter: WalletAdapter | null;
  info: WalletInfo | null;
  /** wallet network differs from the app network (trading must be blocked) */
  networkMismatch: boolean;
  connecting: boolean;
  error: string | null;
  /** counts every change of the wallet's key or network after connect: state bound to an older value is stale */
  epoch: number;
  /** the last account / network change the wallet reported (shown as a banner until dismissed or the next connect) */
  notice: WalletNotice | null;
  dismissNotice(): void;
  connect(id: WalletId): Promise<void>;
  /** opens the private-key signer frame (the key is typed there, never into this page) */
  openPrivateKey(): void;
  disconnect(): void;
  /** placement records of the connected key (memory store when not connected) */
  records: RecordStore;
}

export const WalletContext = createContext<WalletSession | null>(null);

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

export function WalletProvider(props: { services: Services; children: ComponentChildren }) {
  const { config } = props.services;
  const [detected, setDetected] = useState<WalletAdapter[]>([]);
  const [adapter, setAdapter] = useState<WalletAdapter | null>(null);
  const [info, setInfo] = useState<WalletInfo | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);
  const [notice, setNotice] = useState<WalletNotice | null>(null);
  const infoRef = useRef<WalletInfo | null>(null);
  infoRef.current = info;

  // Kaspire: `ordered-args` where the plan is just [signature, dispatch tag] (the wallet then emits the very script kob-wasm assembles: an independent check of the ABI
  // encoding), else `wrap-signature`; finalize verifies every signature either way (real Kaspire 0.5.1 accepts both on TN10)
  const kob = props.services.kob;
  useEffect(() => watchWallets(setDetected, { kastle: config.features.kastle, timeoutMs: 8000, kaspire: { dispatchTag: dispatchTagFrom(kob) } }), [config.features.kastle, kob]);

  // ---- the private-key signer frame (wallet/local-key.ts, ui/shell/SignerFrame.tsx): mounted while its session lives, removing it forgets the key
  const [signerMode, setSignerMode] = useState<SignerMode>('closed');
  const hostRef = useRef<SignerHost | null>(null);
  const { sdk, registry } = props.services;
  const localKey = useMemo(
    () =>
      createLocalKeyAdapter({
        host: () => hostRef.current,
        sdk,
        tokenHints: (built: BuiltTx) => signerTokenHints(kob, built, infoRef.current?.pubkey ?? null, registry),
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

  // Account and network changes after connect: see wallet-events.ts. Listeners live exactly as long as the session.
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
    detected, adapter, info, connecting, error, epoch, notice, dismissNotice, connect, openPrivateKey, disconnect, records,
    networkMismatch: info !== null && info.network !== config.network,
  };
  return (
    <WalletContext.Provider value={session}>
      {props.children}
      <SignerFrame mode={signerMode} events={signerEvents} onHost={(h) => (hostRef.current = h)} onClose={closeSigner} />
    </WalletContext.Provider>
  );
}
