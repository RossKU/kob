// The public node of an app with no node URL configured. It is asked of the community resolvers
// the SDK's `Resolver` uses, several at once.
//
// The SDK asks them one after another, and a resolver that does not answer holds it for about
// 20 s. So the Portfolio's balances could wait 20-40 s for the node. Here a few are asked at once,
// and one more every STAGGER_MS. Once one names a wss:// node, the others already asked get
// COLLECT_MS to answer too, and the rest are aborted. A node named by two or more resolvers is
// taken, otherwise one of the nodes named, at random. So the fastest resolver does not decide
// alone: a hostile one that answers at once wins no more often than the SDK's own pick. The pick
// is kept for the browser session (PICK_TTL_MS), so a reload connects at once. A node that fails
// is forgotten.
//
// A failed node is also remembered for FAILED_TTL_MS, because the resolvers often name the same
// dead node again. A resolver that names one is passed over and the others are asked. The node is
// used only when none names another.
import { normalizeNetwork } from './kaspa-sdk';

/** The resolvers of the SDK's built-in list, the default `[[group]]` of kaspa-wasm `Resolver`. */
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
/** After the first node named, how long the other resolvers asked may still answer. */
export const COLLECT_MS = 300;
/** One resolver's answer is given up after this long. */
export const RESOLVER_TIMEOUT_MS = 4_000;
/** How long a picked node is reused within this browser session. */
export const PICK_TTL_MS = 10 * 60_000;
/** How long a node that failed is passed over. */
export const FAILED_TTL_MS = 5 * 60_000;

const STORE_KEY = 'kob.node.picked';
const FAILED_KEY = 'kob.node.failed';
const MAX_FAILED = 32;

/** Picks a public node for a network from the resolvers. */
export interface PublicNodePicker {
  /**
   * A wss:// node for `network`, or null when no resolver answered. The caller then falls back to
   * the SDK's Resolver.
   */
  pick(network: string): Promise<string | null>;
  /**
   * Records that the node failed. The next pick asks the resolvers again and passes over this node
   * for FAILED_TTL_MS.
   */
  forget(url: string): void;
}

/** Options of `createPublicNodePicker`. */
export interface PickerOptions {
  fetch?: (url: string, init: { signal: AbortSignal }) => Promise<{ ok: boolean; json(): Promise<unknown> }>;
  resolvers?: readonly string[];
  /** Where the pick is kept. Null keeps it in memory only. */
  storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> | null;
  now?: () => number;
  random?: () => number;
  startAtOnce?: number;
  staggerMs?: number;
  collectMs?: number;
  timeoutMs?: number;
}

/** The node URL of one resolver answer (`{"url": "wss://..."}`), or null. Only wss:// is taken. */
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

/** Makes a picker that keeps its pick in session storage. */
export function createPublicNodePicker(o: PickerOptions = {}): PublicNodePicker {
  const fetchFn = o.fetch ?? ((url, init) => fetch(url, { ...init, cache: 'no-store', credentials: 'omit' }));
  const resolvers = o.resolvers ?? PUBLIC_RESOLVERS;
  const storage = o.storage === undefined ? sessionStore() : o.storage;
  const now = o.now ?? Date.now;
  const random = o.random ?? Math.random;
  const atOnce = o.startAtOnce ?? START_AT_ONCE;
  const stagger = o.staggerMs ?? STAGGER_MS;
  const collect = o.collectMs ?? COLLECT_MS;
  const timeout = o.timeoutMs ?? RESOLVER_TIMEOUT_MS;
  let memory: { network: string; url: string; at: number } | null = null;
  // failed node -> when it may be picked again
  let failed: Map<string, number> | null = null;
  const failedNow = (): Map<string, number> => {
    if (!failed) {
      failed = new Map();
      try {
        const v: unknown = JSON.parse(storage?.getItem(FAILED_KEY) ?? 'null');
        if (Array.isArray(v)) for (const e of v.slice(-MAX_FAILED)) if (Array.isArray(e) && typeof e[0] === 'string' && typeof e[1] === 'number') failed.set(e[0], e[1]);
      } catch {
        /* unreadable: none */
      }
    }
    const t = now();
    for (const [u, until] of failed) if (until <= t || until - t > FAILED_TTL_MS) failed.delete(u);
    return failed;
  };
  const isFailed = (url: string) => failedNow().has(url);

  const kept = (network: string): string | null => {
    let k = memory;
    if (!k && storage) {
      try {
        const v = JSON.parse(storage.getItem(STORE_KEY) ?? 'null') as typeof memory;
        if (v && typeof v.network === 'string' && typeof v.at === 'number' && nodeUrlOfAnswer(v) === v.url) k = v;
      } catch {
        /* Unreadable, so ask. */
      }
    }
    return k && k.network === network && now() - k.at < PICK_TTL_MS && now() >= k.at && !isFailed(k.url) ? k.url : null;
  };
  const keep = (network: string, url: string) => {
    memory = { network, url, at: now() };
    try {
      storage?.setItem(STORE_KEY, JSON.stringify(memory));
    } catch {
      /* Memory only. */
    }
  };

  const ask = (network: string): Promise<string | null> => {
    const order = resolvers.map((r) => ({ r, k: random() })).sort((a, b) => a.k - b.k).map((x) => x.r);
    return new Promise((resolve) => {
      const ctls: AbortController[] = [];
      let next = 0;
      let open = 0;
      let done = false;
      // a node that failed lately: used only if no resolver names another
      let fallback: string | null = null;
      let timer: ReturnType<typeof setInterval> | null = null;
      // the nodes named so far, with how many resolvers named each, in the order first named
      const named = new Map<string, number>();
      let collecting: ReturnType<typeof setTimeout> | null = null;
      const finish = (url: string | null) => {
        if (done) return;
        done = true;
        if (timer) clearInterval(timer);
        if (collecting) clearTimeout(collecting);
        for (const c of ctls) c.abort();
        resolve(url);
      };
      // a node two resolvers agree on, otherwise one of those named, at random
      const decide = () => {
        const most = Math.max(...named.values());
        const pool = [...named].filter(([, n]) => n === most).map(([u]) => u);
        finish(pool[Math.min(pool.length - 1, Math.floor(random() * pool.length))]!);
      };
      const launch = () => {
        if (done || collecting || next >= order.length) return;
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
            if (done) return;
            if (url && !isFailed(url)) {
              named.set(url, (named.get(url) ?? 0) + 1);
              // the first node named: no more resolvers are started, those asked get COLLECT_MS
              if (!collecting) {
                if (timer) clearInterval(timer);
                collecting = setTimeout(decide, collect);
              }
            } else if (url) fallback ??= url;
            if (collecting) {
              if (open === 0) decide();
            } else if (open === 0 && next >= order.length) finish(fallback);
            else launch(); // a failed resolver, or one naming a failed node, hands its turn on at once
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
      const f = failedNow();
      f.delete(url);
      f.set(url, now() + FAILED_TTL_MS);
      try {
        storage?.setItem(FAILED_KEY, JSON.stringify([...f].slice(-MAX_FAILED)));
      } catch {
        /* memory only */
      }
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
