// Composition root: builds every service the UI needs from an `AppConfig`. The UI never constructs data-layer objects itself; it receives
// `Services` (through the Preact context in app/context.tsx) so tests can substitute any part.
import { indexerKey, type AppConfig } from '../config';
import { createFeeService, type FeeService } from '../kob/fee-policy';
import { HttpIndexer, IndexerFeed, type IndexerApi } from '../data/indexer';
import { IndexerFailover, failoverIndexer, pinnedIndexer } from '../data/indexer-failover';
import { loadKaspaSdk, type KaspaSdk } from '../data/kaspa-sdk';
import type { NodeApi } from '../data/node';
import { createNode } from '../data/node-factory';
import { TokenTracker } from '../data/token-tracker';
import { createUtxoService, type UtxoService } from '../data/utxos';
import { parseRegistry, type TokenRegistry } from '../kob/registry';
import type { KobWasm } from '../kob/wasm';
import { loadKob } from '../kob/wasm.browser';

export interface Services {
  config: AppConfig;
  kob: KobWasm;
  sdk: KaspaSdk;
  node: NodeApi;
  /**
   * null when `config.indexerUrl` is empty: market/orders views degrade to node-only features. With `extraIndexerUrls` this is the ACTIVE indexer: the
   * primary while it is healthy, else a healthy backup (see `failover`); one call goes to one indexer.
   */
  indexer: IndexerApi | null;
  feed: IndexerFeed | null;
  /** empty registry (no tokens) when loading failed: see `registryError` */
  registry: TokenRegistry;
  registryError: string | null;
  /**
   * the indexers key facts are cross-checked against: the further indexers (config `extraIndexerUrls`) while the primary is active; on a backup, the
   * other ones and the primary while it answers. Empty = a single indexer, unverified. A getter with failover: read it when it is used.
   */
  verifiers: { label: string; api: IndexerApi }[];
  /** the failover between the primary indexer and the further ones (null: a single indexer, or one given by the caller) */
  failover: IndexerFailover | null;
  /** `indexer` as it is NOW (not the proxy): a read of several parts that must come from one indexer pins it first */
  pinnedIndexer(): IndexerApi | null;
  utxos: UtxoService;
  tracker: TokenTracker;
  /** the dynamic fee policy (from `config.fees`) and the oracle that reads the node's fee estimate */
  fees: FeeService;
}

export interface CreateServicesOverrides {
  kob?: KobWasm;
  sdk?: KaspaSdk;
  node?: NodeApi;
  indexer?: IndexerApi | null;
  feed?: IndexerFeed | null;
  verifiers?: { label: string; api: IndexerApi }[];
  fees?: FeeService;
  fetch?: typeof fetch;
}

async function loadRegistry(
  config: AppConfig,
  kob: KobWasm,
  fetchFn: typeof fetch,
): Promise<{ registry: TokenRegistry; error: string | null }> {
  const empty = (network: string): TokenRegistry => ({ network, templates: [], templateChecks: [], tokens: [], byCovenantId: new Map() });
  try {
    const res = await fetchFn(config.registryUrl, { cache: 'no-cache' });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return { registry: parseRegistry(await res.text(), { kob, network: config.network }), error: null };
  } catch (e) {
    return { registry: empty(config.network), error: e instanceof Error ? e.message : String(e) };
  }
}

/** The fee policy of `config.fees` and, when it is dynamic, an oracle over the node's `getFeeEstimate` (a node without it answers null: the floor). */
export function feeServiceFor(config: Pick<AppConfig, 'fees'>, node: Pick<NodeApi, 'getFeeEstimate'>): FeeService {
  return createFeeService(config.fees, node);
}

/** The further indexers that are not the primary one under another spelling (`indexerKey`). */
export function independentIndexers(config: Pick<AppConfig, 'indexerUrl' | 'extraIndexerUrls'>): string[] {
  const primary = config.indexerUrl ? indexerKey(config.indexerUrl) : null;
  return config.extraIndexerUrls.filter((u) => indexerKey(u) !== primary);
}

/** Loads kob-wasm and the kaspa SDK, connects the node (lazily: a failing node does not block the UI), reads the registry. */
export async function createServices(config: AppConfig, o: CreateServicesOverrides = {}): Promise<Services> {
  const fetchFn = o.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  const [kob, sdk] = await Promise.all([
    o.kob ? Promise.resolve(o.kob) : loadKob(),
    o.sdk ? Promise.resolve(o.sdk) : loadKaspaSdk({ exposeForTests: config.features.test }),
  ]);
  // the offline mock node (an http(s) node URL) only in a test build or on the vite dev server
  const node = o.node ?? createNode({ network: config.network, nodeUrl: config.nodeUrl, allowMock: config.features.test || import.meta.env.DEV === true }, sdk);
  // the primary indexer in another spelling (case, default port, trailing slash) is not an independent verifier
  const extras = independentIndexers(config);
  // failover (data/indexer-failover.ts) only for the indexers the configuration names: a caller that supplies its own indexer, feed or verifiers keeps them
  const withFailover = !!config.indexerUrl && extras.length > 0 && o.indexer === undefined && o.feed === undefined && o.verifiers === undefined;
  const primaryApi: IndexerApi | null = config.indexerUrl ? new HttpIndexer({ baseUrl: config.indexerUrl }) : null;
  const extraApis = extras.map((u) => ({ label: u, api: new HttpIndexer({ baseUrl: u }) as IndexerApi }));
  let indexer: IndexerApi | null = o.indexer !== undefined ? o.indexer : primaryApi;
  const feed = o.feed !== undefined ? o.feed : config.indexerUrl ? new IndexerFeed({ url: config.indexerUrl }) : null;
  let failover: IndexerFailover | null = null;
  const verifiers: { label: string; api: IndexerApi }[] = o.verifiers ?? extraApis;
  let verifiersOf = (): { label: string; api: IndexerApi }[] => verifiers;
  let pinned = (): IndexerApi | null => indexer;
  if (withFailover && primaryApi && feed) {
    const apis = [primaryApi, ...extraApis.map((e) => e.api)];
    const labels = [config.indexerUrl, ...extraApis.map((e) => e.label)];
    const f = new IndexerFailover({ members: labels.map((label, i) => ({ label, api: apis[i]! })), network: config.network });
    failover = f;
    indexer = failoverIndexer(f, apis);
    pinned = () => pinnedIndexer(f, apis);
    // the feed follows the active indexer: its first open there is a reconnect with a resync, so every live view reads again
    f.subscribe((st) => feed.retarget(labels[st.active]!));
    // one array per set of verifiers (a view's dependency list sees a change only when the set changes)
    let cache: { key: string; list: { label: string; api: IndexerApi }[] } = { key: '', list: [] };
    verifiersOf = () => {
      const idx = f.state.verifiers;
      const key = idx.join(',');
      if (cache.key !== key || cache.list.length !== idx.length) cache = { key, list: idx.map((i) => ({ label: labels[i]!, api: apis[i]! })) };
      return cache.list;
    };
  }
  const { registry, error } = await loadRegistry(config, kob, fetchFn);
  const utxos = createUtxoService({ node, sdk, network: config.network });
  const tracker = new TokenTracker({ kob, node, sdk, network: config.network, indexer });
  return {
    config, kob, sdk, node, indexer, feed, registry, registryError: error,
    get verifiers() {
      return verifiersOf();
    },
    failover, pinnedIndexer: pinned, utxos, tracker, fees: o.fees ?? feeServiceFor(config, node),
  };
}
