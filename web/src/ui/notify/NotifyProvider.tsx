// Notification provider: owns the settings (opt-in, `kob.notify.settings`), the per-wallet store, and the polling that feeds it. Nothing runs while the
// notifications are off (the default) or no wallet is connected: no request, no feed subscription. When on, the wallet's orders and incoming payments are
// polled every 15 s and on every live `fills` notice of the indexer feed, watched x402 invoices every 30 s (see poll.ts).
//
// Delivery of a new event: always an in-app toast (a burst of more than 3 collapses into one summary toast); additionally a browser notification when
// `settings.browser` is on, the permission was granted (it is requested only from a click in Settings) and the page is HIDDEN: a visible page already
// shows the toast, so the OS notification is for the background tab. The notification `tag` is the event id (the browser merges repeats).
import { createContext, type ComponentChildren } from 'preact';
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { t } from '../../i18n';
import {
  NotificationStore, loadNotifySettings, parseInvoiceUrl, safeStorage, saveNotifySettings, type NotificationEvent, type NotifyKind, type NotifySettings,
} from '../../kob/notifications';
import { showToast, useInterval, type ToastTone } from '../kit';
import { describeEvent } from './describe';
import { INVOICE_POLL_MS, ORDERS_POLL_MS, pollInvoices, pollWallet } from './poll';
import { useLiveRefresh } from '../market/live';

export type BrowserPermission = 'granted' | 'denied' | 'default' | 'unsupported';

export interface NotifyApi {
  settings: NotifySettings;
  /** merges a patch (kinds merged key by key) and persists */
  update(patch: { enabled?: boolean; browser?: boolean; kinds?: Partial<Record<NotifyKind, boolean>> }): void;
  items: readonly NotificationEvent[];
  unread: number;
  /** a wallet is connected (notifications are per wallet) */
  connected: boolean;
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
  settings: loadNotifySettings(null), update() {}, items: [], unread: 0, connected: false, markAllRead() {}, clear() {}, invoices: [], addInvoice: () => 'invalid', removeInvoice() {},
  permission: browserPermission, requestBrowser: async () => browserPermission(),
};

export const NotifyContext = createContext<NotifyApi>(STUB);
export const useNotify = (): NotifyApi => useContext(NotifyContext);

const TONE: Partial<Record<NotifyKind, ToastTone>> = { fill: 'ok', payment: 'ok', expirySoon: 'warn', expired: 'warn', killed: 'warn' };

function deliver(fresh: readonly NotificationEvent[], s: NotifySettings, registry: Parameters<typeof describeEvent>[1]): void {
  const shown = fresh.length > 3 ? fresh.slice(0, 1) : fresh;
  for (const e of shown) {
    const d = describeEvent(e, registry);
    showToast(`${d.title}: ${d.body}`, TONE[e.kind] ?? 'info');
  }
  if (fresh.length > 3) showToast(t('notify.more', { count: fresh.length - 1 }), 'info');
  if (!s.browser || browserPermission() !== 'granted' || typeof document === 'undefined' || !document.hidden) return;
  for (const e of fresh.slice(0, 5)) {
    const d = describeEvent(e, registry);
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
      if (fresh.length) deliver(fresh, s, services.registry);
    },
    [store, services.registry],
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
  useInterval(() => void poll(), ORDERS_POLL_MS, active);
  useLiveRefresh(services, active ? ['fills'] : [], () => active, () => void poll(), 60_000);

  const pollInv = useCallback(async () => {
    if (store) ingest(await pollInvoices(store, Date.now()));
  }, [store, ingest]);
  useEffect(() => {
    if (active) void pollInv();
  }, [active, pollInv]);
  useInterval(() => void pollInv(), INVOICE_POLL_MS, active);

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

  const api: NotifyApi = {
    settings,
    update,
    items: store?.items() ?? [],
    unread: store?.unread() ?? 0,
    connected: !!store,
    markAllRead: () => store?.markAllRead(),
    clear: () => store?.clear(),
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
