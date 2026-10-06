// Type declarations of the mock server (implementation: server.mjs, plain node ESM).
import type { MockChain } from './chain.mjs';

export interface MockServerOptions {
  port?: number;
  host?: string;
  /** apply the default seed (fictional TN10 token, book, fills); default true */
  seed?: boolean;
  /** give the dev keys alice and bob KAS and tokens */
  fund?: boolean;
  /** opt-in seeded market history (48 h of trades by default, see seedHistory in seed.mjs); default false */
  history?: boolean | import('./seed.mjs').HistorySpec;
  /** opt-in token/token pair seed (EXKCC/EXUSD pair orders of every kind + an EXUSD KAS book, see seedPair in pairs.mjs); default false */
  pair?: boolean | Record<string, unknown>;
  settleDepthDaa?: number;
  /** DAA per wall-clock second (default 10; 0 freezes the clock) */
  daaPerSecond?: number;
  startDaa?: number;
  network?: string;
  healthEveryMs?: number;
  log?: (line: string) => void;
}

export interface MockServer {
  /** `http://host:port` (no trailing slash): indexer under `/v1`, node under `/node`, control under `/mock`, WebSocket at `/v1/ws` */
  url: string;
  port: number;
  chain: MockChain;
  /** alias of `chain` */
  state: MockChain;
  wsClients(): number;
  close(): Promise<void>;
}

export function startMockServer(opts?: MockServerOptions): Promise<MockServer>;
export function fundDevKeys(chain: MockChain): void;
export function parseChannel(s: string): string;
export function handleOp(text: string, subs: Set<string>): { type: string; data?: unknown };
export function framesFor(ev: unknown, subs: Set<string>): { channel: string; type: string; data: unknown }[];
