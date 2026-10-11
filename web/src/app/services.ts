// Composition root: builds every service the UI needs from an `AppConfig`. The UI never constructs data-layer objects itself; it receives
// `Services` (through the Preact context in app/context.tsx) so tests can substitute any part.
import { indexerKey, type AppConfig } from '../config';
import { createFeeService, type FeeService } from '../kob/fee-policy';
import { HttpIndexer, IndexerFeed, type IndexerApi } from '../data/indexer';
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
  /** null when `config.indexerUrl` is empty: market/orders views degrade to node-only features */
  indexer: IndexerApi | null;
  feed: IndexerFeed | null;
  /** empty registry (no tokens) when loading failed: see `registryError` */
  registry: TokenRegistry;
  registryError: string | null;
  /** further indexers (config `extraIndexerUrls`) that key facts are cross-checked against; empty = single indexer, unverified */
  verifiers: { label: string; api: IndexerApi }[];
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
  const indexer = o.indexer !== undefined ? o.indexer : config.indexerUrl ? new HttpIndexer({ baseUrl: config.indexerUrl }) : null;
  const feed = o.feed !== undefined ? o.feed : config.indexerUrl ? new IndexerFeed({ url: config.indexerUrl }) : null;
  // the primary indexer in another spelling (case, default port, trailing slash) is not an independent verifier
  const verifiers = o.verifiers ?? independentIndexers(config).map((u) => ({ label: u, api: new HttpIndexer({ baseUrl: u }) as IndexerApi }));
  const { registry, error } = await loadRegistry(config, kob, fetchFn);
  const utxos = createUtxoService({ node, sdk, network: config.network });
  const tracker = new TokenTracker({ kob, node, sdk, network: config.network, indexer });
  return { config, kob, sdk, node, indexer, feed, registry, registryError: error, verifiers, utxos, tracker, fees: o.fees ?? feeServiceFor(config, node) };
}
