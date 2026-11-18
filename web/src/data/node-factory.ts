// Picks the `NodeApi` implementation by URL scheme.
// - '' or ws:// or wss:// selects KaspaRpcNode, the official SDK over wRPC. An empty URL means a
//   public node the resolvers name (data/public-node.ts).
// - http:// or https:// selects HttpMockNode, the offline mock server and never a real node (see
//   node-http.ts). It is allowed only with `allowMock`, in a test build (`features.test`) or the
//   vite dev server. Elsewhere such a URL is refused, because a plain JSON server would otherwise
//   stand in for the node, for input facts and acceptance, on any network.
import type { NodeApi } from './node';
import type { KaspaSdk } from './kaspa-sdk';
import { KaspaRpcNode, type KaspaRpcNodeOptions } from './node-rpc';
import { HttpMockNode, type HttpMockNodeOptions } from './node-http';
import { createPublicNodePicker, type PublicNodePicker } from './public-node';

let picker: PublicNodePicker | null = null;
/**
 * The one picker of the page. A node without a URL connects to the public node it picked, and
 * services.ts asks it early.
 */
export const publicNodePicker = (): PublicNodePicker => (picker ??= createPublicNodePicker());

/** The node settings of the app. */
export interface NodeConfig {
  network: string;
  nodeUrl: string;
  /**
   * Whether an http(s) URL is accepted as the offline mock node. It is for test and development
   * builds only, and false by default.
   */
  allowMock?: boolean;
}

/** Thrown for an http(s) node URL in a build that does not allow the mock node. */
export class MockNodeRefused extends Error {
  constructor(url: string) {
    super(`the node URL ${url.slice(0, 64)} selects the offline mock node, which only test and development builds use: use a ws:// or wss:// node, or leave it empty for the public resolver`);
    this.name = 'MockNodeRefused';
  }
}

/** Options passed through to the node implementation. */
export interface CreateNodeExtras {
  rpc?: Partial<KaspaRpcNodeOptions>;
  http?: Partial<HttpMockNodeOptions>;
}

/** Which `NodeApi` implementation a URL selects. */
export type NodeKind = 'rpc' | 'http-mock';

/** The implementation a node URL selects by its scheme. It throws for another scheme. */
export function nodeKindFor(nodeUrl: string): NodeKind {
  const u = nodeUrl.trim().toLowerCase();
  if (u === '' || u.startsWith('ws://') || u.startsWith('wss://')) return 'rpc';
  if (u.startsWith('http://') || u.startsWith('https://')) return 'http-mock';
  throw new Error(`unsupported node URL scheme: ${nodeUrl.slice(0, 32)} (use ws://, wss://, http:// or https://)`);
}

/**
 * Makes the node the config names. The SDK is only required for the wRPC node, so pass `null`
 * when the URL is http(s).
 */
export function createNode(config: NodeConfig, sdk: KaspaSdk | null, extras: CreateNodeExtras = {}): NodeApi {
  const url = config.nodeUrl.trim();
  if (nodeKindFor(url) === 'http-mock') {
    if (!config.allowMock) throw new MockNodeRefused(url);
    return new HttpMockNode({ baseUrl: url, network: config.network, ...extras.http });
  }
  if (!sdk) throw new Error('createNode: the kaspa SDK is required for a wRPC node');
  const publicNode = url ? {} : { publicNode: publicNodePicker() };
  return new KaspaRpcNode({ sdk, network: config.network, url, ...publicNode, ...extras.rpc });
}
