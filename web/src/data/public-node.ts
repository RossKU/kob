// The public node of an app with no node URL configured: asked of the community resolvers the SDK's `Resolver` uses, several at once.
// The SDK asks them one after another, and a resolver that does not answer holds it for about 20 s (TN10 2026-11-17: 5 of the 16 hung,
// 3 answered an error), so the Portfolio's balances waited 20-40 s for the node. Here a few are asked at once, one more every STAGGER_MS, and
// the first that names a wss:// node wins; the others are aborted. The node it names is no more trusted than the SDK's pick (the same
// resolvers). The pick is kept for the browser session (PICK_TTL_MS) so a reload connects at once; a node that fails is forgotten.
import { normalizeNetwork } from './kaspa-sdk';

/** The resolvers of the SDK's built-in list (kaspa-wasm `Resolver`, its default `[[group]]` entries). */
export const PUBLIC_RESOLVERS: readonly string[] = [
  ...['eric', 'maxim', 'sean', 'troy'].map((n) => `https://${n}.kaspa.stream`),
  ...['john', 'mike', 'paul', 'alex'].map((n) => `https://${n}.kaspa.red`),
  ...['jake', 'mark', 'adam', 'liam'].map((n) => `https://${n}.kaspa.green`),
  ...['noah', 'ryan', 'jack', 'luke'].map((n) => `https://${n}.kaspa.blue`),
];

/** Resolvers asked at once. */
export const START_AT_ONCE = 4;
/** Then one more every this long while none has answered. */
export const STAGGER_MS = 300;
/** One resolver's answer is given up after this long. */
export const RESOLVER_TIMEOUT_MS = 4_000;
/** How long a picked node is reused (this browser session). */
export const PICK_TTL_MS = 10 * 60_000;

const STORE_KEY = 'kob.node.picked';

export interface PublicNodePicker {
  /** a wss:// node for `network`, or null when no resolver answered (the caller falls back to the SDK's Resolver) */
  pick(network: string): Promise<string | null>;
  /** the node failed: the next pick asks the resolvers again */
  forget(url: string): void;
}

export interface PickerOptions {
  fetch?: (url: string, init: { signal: AbortSignal }) => Promise<{ ok: boolean; json(): Promise<unknown> }>;
  resolvers?: readonly string[];
  /** where the pick is kept; null = memory only */
  storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> | null;
  now?: () => number;
  random?: () => number;
  startAtOnce?: number;
  staggerMs?: number;
  timeoutMs?: number;
}

/** The node URL of one resolver answer (`{"url": "wss://..."}`), or null. */
export function nodeUrlOfAnswer(body: unknown): string | null {
  const url = (body as { url?: unknown } | null)?.url;
  if (typeof url !== 'string' || url.length > 512) return null;
  try {
    const u = new URL(url);
    return u.protocol === 'wss:' && !u.username && !u.password ? url : null;
  } catch {
    return null;
  }
}

const sessionStore = (): PickerOptions['storage'] => {
  try {
    return typeof sessionStorage === 'undefined' ? null : sessionStorage;
  } catch {
    return null;
  }
};

export function createPublicNodePicker(o: PickerOptions = {}): PublicNodePicker {
  const fetchFn = o.fetch ?? ((url, init) => fetch(url, { ...init, cache: 'no-store', credentials: 'omit' }));
  const resolvers = o.resolvers ?? PUBLIC_RESOLVERS;
  const storage = o.storage === undefined ? sessionStore() : o.storage;
  const now = o.now ?? Date.now;
  const random = o.random ?? Math.random;
  const atOnce = o.startAtOnce ?? START_AT_ONCE;
  const stagger = o.staggerMs ?? STAGGER_MS;
  const timeout = o.timeoutMs ?? RESOLVER_TIMEOUT_MS;
  let memory: { network: string; url: string; at: number } | null = null;

  const kept = (network: string): string | null => {
    let k = memory;
    if (!k && storage) {
      try {
        const v = JSON.parse(storage.getItem(STORE_KEY) ?? 'null') as typeof memory;
        if (v && typeof v.network === 'string' && typeof v.at === 'number' && nodeUrlOfAnswer(v) === v.url) k = v;
      } catch {
        /* unreadable: ask */
      }
    }
    return k && k.network === network && now() - k.at < PICK_TTL_MS && now() >= k.at ? k.url : null;
  };
  const keep = (network: string, url: string) => {
    memory = { network, url, at: now() };
    try {
      storage?.setItem(STORE_KEY, JSON.stringify(memory));
    } catch {
      /* memory only */
    }
  };

  const ask = (network: string): Promise<string | null> => {
    const order = resolvers.map((r) => ({ r, k: random() })).sort((a, b) => a.k - b.k).map((x) => x.r);
    return new Promise((resolve) => {
      const ctls: AbortController[] = [];
      let next = 0;
      let open = 0;
      let done = false;
      let timer: ReturnType<typeof setInterval> | null = null;
      const finish = (url: string | null) => {
        if (done) return;
        done = true;
        if (timer) clearInterval(timer);
        for (const c of ctls) c.abort();
        resolve(url);
      };
      const launch = () => {
        if (done || next >= order.length) return;
        const base = order[next++]!.replace(/\/+$/, '');
        const ctl = new AbortController();
        ctls.push(ctl);
        const t = setTimeout(() => ctl.abort(), timeout);
        open++;
        fetchFn(`${base}/v2/kaspa/${network}/tls/wrpc/borsh`, { signal: ctl.signal })
          .then((r) => (r.ok ? r.json() : null))
          .then((body) => nodeUrlOfAnswer(body))
          .catch(() => null)
          .then((url) => {
            clearTimeout(t);
            open--;
            if (url) finish(url);
            else if (open === 0 && next >= order.length) finish(null);
            else launch(); // a failed resolver hands its turn on at once
          });
      };
      for (let i = 0; i < Math.min(atOnce, order.length); i++) launch();
      if (order.length === 0) finish(null);
      timer = setInterval(() => (next < order.length ? launch() : timer && clearInterval(timer)), stagger);
    });
  };

  let inflight: { network: string; p: Promise<string | null> } | null = null;
  return {
    pick(network) {
      const net = normalizeNetwork(network);
      const k = kept(net);
      if (k) return Promise.resolve(k);
      if (inflight?.network === net) return inflight.p;
      const p = ask(net).then((url) => {
        if (url) keep(net, url);
        return url;
      });
      inflight = { network: net, p: p.finally(() => (inflight = null)) };
      return inflight.p;
    },
    forget(url) {
      if (memory?.url === url) memory = null;
      try {
        const v = JSON.parse(storage?.getItem(STORE_KEY) ?? 'null') as { url?: string } | null;
        if (v?.url === url) storage?.removeItem(STORE_KEY);
      } catch {
        /* ignore */
      }
    },
  };
}
