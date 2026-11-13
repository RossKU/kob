// Notification provider: owns the settings (opt-in, `kob.notify.settings`), the per-wallet store, and the polling that feeds it. Nothing runs while the
// notifications are off (the default) or no wallet is connected: no request, no feed subscription. When on, the wallet's orders and incoming payments are
// polled every 15 s and on every live `fills` notice of the indexer feed, watched x402 invoices every 30 s (see poll.ts).
//
// Delivery of a new event: always an in-app toast (a burst of more than 3 collapses into one summary toast); additionally a browser notification when
// `settings.browser` is on, the permission was granted (it is requested only from a click in Settings) and the page is HIDDEN: a visible page already
// shows the toast, so the OS notification is for the background tab. The notification `tag` is the event id (the browser merges repeats).
//
// System notifications (system-notices.ts: the market switched to the node, and back) need no wallet and no opt-in: they are in the bell's list and count,
// for this session.
import { createContext, type ComponentChildren } from 'preact';
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import {
  NotificationStore, loadNotifySettings, parseInvoiceUrl, safeStorage, saveNotifySettings, type NotificationEvent, type NotifyKind, type NotifySettings,
} from '../../kob/notifications';
import { showToast, useInterval, type ToastTone } from '../kit';
import type { Fiat } from '../../app/fiat';
import { currentFiat } from '../../app/use-fiat';
import { describeEvent } from './describe';
import { INVOICE_POLL_MS, ORDERS_POLL_MS, pollInvoices, pollWallet } from './poll';
import { declaredToken, loadDeclaration, restoreDeclarations } from '../../data/token-declarations';
import { useLiveRefresh } from '../market/live';
import { addNotice, noticeOfSwitch, type SystemNotice } from './system-notices';

export type BrowserPermission = 'granted' | 'denied' | 'default' | 'unsupported';

export interface NotifyApi {
  settings: NotifySettings;
  /** merges a patch (kinds merged key by key) and persists */
  update(patch: { enabled?: boolean; browser?: boolean; kinds?: Partial<Record<NotifyKind, boolean>> }): void;
  items: readonly NotificationEvent[];
  /** the app's own notifications (no wallet needed), newest first */
  system: readonly SystemNotice[];
  /** unread notifications of the wallet and of the app */
  unread: number;
  /** a wallet is connected (notifications are per wallet) */
  connected: boolean;
  markRead(id: string): void;
  markAllRead(): void;
  clear(): void;
  invoices: ReturnType<NotificationStore['invoices']>;
  addInvoice(input: string): 'ok' | 'invalid' | 'duplicate';
  removeInvoice(id: string): void;
  /** current browser permission; `requestBrowser` must be called from a click */
  permission(): BrowserPermission;
  requestBrowser(): Promise<BrowserPermission>;
}

export const browserPermission = (): BrowserPermission => (typeof Notification === 'undefined' ? 'unsupported' : Notification.permission);

const STUB: NotifyApi = {
  settings: loadNotifySettings(null), update() {}, items: [], system: [], unread: 0, connected: false, markRead() {}, markAllRead() {}, clear() {}, invoices: [], addInvoice: () => 'invalid', removeInvoice() {},
  permission: browserPermission, requestBrowser: async () => browserPermission(),
};

export const NotifyContext = createContext<NotifyApi>(STUB);
export const useNotify = (): NotifyApi => useContext(NotifyContext);

/** How long a toast waits for the declaration of a token it names (data/token-declarations.ts); then it shows what is known. */
const DECLARATION_WAIT_MS = 2500;

const TONE: Partial<Record<NotifyKind, ToastTone>> = { fill: 'ok', payment: 'ok', expirySoon: 'warn', expired: 'warn', killed: 'warn' };

function deliver(fresh: readonly NotificationEvent[], s: NotifySettings, registry: Parameters<typeof describeEvent>[1], fiat: Fiat | null, network: string): void {
  const shown = fresh.length > 3 ? fresh.slice(0, 1) : fresh;
  for (const e of shown) {
    const d = describeEvent(e, registry, fiat, network);
    showToast(`${d.title}: ${d.body}`, TONE[e.kind] ?? 'info');
  }
  if (fresh.length > 3) showToast(t('notify.more', { count: fresh.length - 1 }), 'info');
  if (!s.browser || browserPermission() !== 'granted' || typeof document === 'undefined' || !document.hidden) return;
  for (const e of fresh.slice(0, 5)) {
    const d = describeEvent(e, registry, fiat, network);
    try {
      new Notification(d.title, { body: d.body, tag: e.id });
    } catch {
      /* some mobile browsers only allow notifications through a service worker */
    }
  }
}

export function NotifyProvider(props: { children: ComponentChildren }) {
  const services = useServices();
  const wallet = useWallet();
  const pubkey = wallet.info?.pubkey ?? null;
  const network = services.config.network;
  const [settings, setSettings] = useState<NotifySettings>(() => loadNotifySettings());
  const [, setTick] = useState(0);
  const [system, setSystem] = useState<SystemNotice[]>([]);
  // one notification per switch of the market to the node and back (the state the page opened with is not a switch)
  useEffect(() => {
    const c = services.standalone;
    if (!c) return undefined;
    let prev = c.state;
    return c.subscribe((next) => {
      const n = noticeOfSwitch(prev, next, Date.now());
      prev = next;
      if (n) setSystem((l) => addNotice(l, n));
    });
  }, [services.standalone]);

  const store = useMemo(() => (pubkey ? new NotificationStore(safeStorage(), network, pubkey) : null), [pubkey, network]);
  useEffect(() => (store ? store.subscribe(() => setTick((n) => n + 1)) : undefined), [store]);

  const active = !!store && settings.enabled && !wallet.networkMismatch;
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const inflight = useRef(false);

  const ingest = useCallback(
    (events: readonly NotificationEvent[]) => {
      if (!store || !events.length) return;
      const s = settingsRef.current;
      const fresh = store.ingest(events, (k) => s.kinds[k]);
      if (!fresh.length) return;
      const network = services.config.network;
      const show = () => deliver(fresh, s, services.registry, currentFiat(network === 'testnet-10'), network);
      // a token the page has not met yet has no name or decimals: its declaration is asked for first (a moment at most), so the toast reads "26.6325 TUSD", not raw units
      const unknown = [...new Set(fresh.filter((e) => e.kind === 'payment' && e.token && !services.registry.byCovenantId.has(e.token) && declaredToken(network, e.token) === undefined).map((e) => e.token!))];
      if (!unknown.length) return show();
      restoreDeclarations(services.kob);
      const asked = Promise.all(unknown.map((id) => loadDeclaration(services.kob, services.indexer, network, id)));
      void Promise.race([asked, new Promise((r) => setTimeout(r, DECLARATION_WAIT_MS))]).then(show, show);
    },
    [store, services.registry, services.kob, services.indexer, services.config.network],
  );

  const poll = useCallback(async () => {
    if (!store || !pubkey || inflight.current) return;
    inflight.current = true;
    try {
      const { events } = await pollWallet(services, store, pubkey, Date.now());
      ingest(events);
    } finally {
      inflight.current = false;
    }
  }, [store, pubkey, services, ingest]);

  useEffect(() => {
    if (active) void poll();
  }, [active, poll]);
  // notifications are for the moments the page is not looked at: these polls keep running in a background tab
  useInterval(() => void poll(), ORDERS_POLL_MS, active, { background: true });
  useLiveRefresh(services, active ? ['fills'] : [], () => active, () => void poll(), 60_000, { background: true });

  const pollInv = useCallback(async () => {
    if (store) ingest(await pollInvoices(store, Date.now()));
  }, [store, ingest]);
  useEffect(() => {
    if (active) void pollInv();
  }, [active, pollInv]);
  useInterval(() => void pollInv(), INVOICE_POLL_MS, active, { background: true });

  const update = useCallback<NotifyApi['update']>((patch) => {
    setSettings((cur) => {
      const next: NotifySettings = { enabled: patch.enabled ?? cur.enabled, browser: patch.browser ?? cur.browser, kinds: { ...cur.kinds, ...patch.kinds } };
      saveNotifySettings(next);
      return next;
    });
  }, []);

  const requestBrowser = useCallback(async (): Promise<BrowserPermission> => {
    if (typeof Notification === 'undefined') return 'unsupported';
    if (Notification.permission !== 'default') return Notification.permission;
    try {
      return (await Notification.requestPermission()) as BrowserPermission;
    } catch {
      return Notification.permission;
    }
  }, []);

  const systemUnread = system.filter((n) => !n.read).length;
  const api: NotifyApi = {
    settings,
    update,
    items: store?.items() ?? [],
    system,
    unread: (store?.unread() ?? 0) + systemUnread,
    connected: !!store,
    markRead: (id) => {
      if (system.some((n) => n.id === id)) setSystem((l) => l.map((n) => (n.id === id ? { ...n, read: true } : n)));
      else store?.markRead(id);
    },
    markAllRead: () => {
      setSystem((l) => (l.some((n) => !n.read) ? l.map((n) => ({ ...n, read: true })) : l));
      store?.markAllRead();
    },
    clear: () => {
      setSystem([]);
      store?.clear();
    },
    invoices: store?.invoices() ?? [],
    addInvoice: (input) => {
      const p = parseInvoiceUrl(input);
      if (!p || !store) return 'invalid';
      if (!store.addInvoice({ url: p.url, id: p.id, reference: null, status: null, addedAt: Date.now() })) return 'duplicate';
      void pollInv();
      return 'ok';
    },
    removeInvoice: (id) => store?.removeInvoice(id),
    permission: browserPermission,
    requestBrowser,
  };
  return <NotifyContext.Provider value={api}>{props.children}</NotifyContext.Provider>;
}
