// Picks the `NodeApi` implementation by URL scheme:
//   ''  or  ws:// / wss://   -> KaspaRpcNode (official SDK over wRPC; '' = the SDK's public Resolver)
//   http:// / https://       -> HttpMockNode  (the offline mock server; never a real node, see node-http.ts)
import type { NodeApi } from './node';
import type { KaspaSdk } from './kaspa-sdk';
import { KaspaRpcNode, type KaspaRpcNodeOptions } from './node-rpc';
import { HttpMockNode, type HttpMockNodeOptions } from './node-http';

export interface NodeConfig {
  network: string;
  nodeUrl: string;
}

export interface CreateNodeExtras {
  rpc?: Partial<KaspaRpcNodeOptions>;
  http?: Partial<HttpMockNodeOptions>;
}

export type NodeKind = 'rpc' | 'http-mock';

export function nodeKindFor(nodeUrl: string): NodeKind {
  const u = nodeUrl.trim().toLowerCase();
  if (u === '' || u.startsWith('ws://') || u.startsWith('wss://')) return 'rpc';
  if (u.startsWith('http://') || u.startsWith('https://')) return 'http-mock';
  throw new Error(`unsupported node URL scheme: ${nodeUrl.slice(0, 32)} (use ws://, wss://, http:// or https://)`);
}

/** The SDK is only required for the wRPC node: pass `null` when the URL is http(s). */
export function createNode(config: NodeConfig, sdk: KaspaSdk | null, extras: CreateNodeExtras = {}): NodeApi {
  const url = config.nodeUrl.trim();
  if (nodeKindFor(url) === 'http-mock') return new HttpMockNode({ baseUrl: url, network: config.network, ...extras.http });
  if (!sdk) throw new Error('createNode: the kaspa SDK is required for a wRPC node');
  return new KaspaRpcNode({ sdk, network: config.network, url, ...extras.rpc });
}
