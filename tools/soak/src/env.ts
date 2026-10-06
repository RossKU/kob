// Process-wide services: kob-wasm, the official kaspa SDK, one node connection, the indexer client, keys and the soak state file.
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createKob, type KobWasm, type RawKobWasm } from '@/kob/wasm';
import type { KaspaSdk } from '@/data/kaspa-sdk';
import { KaspaRpcNode } from '@/data/node-rpc';
import { HttpIndexer } from '@/data/indexer';
import { createFeeService, type FeeService } from '@/kob/fee-policy';
import type { LoadedConfig, TokenSlot } from './config';
import type { IndexerGate } from './indexer-gate';

export interface KeyEntry {
  secretKey: string;
  publicKey: string;
  address: string;
}

/** An issued soak token (run/state.json). */
export interface IssuedToken {
  covenantId: string;
  issueTxid: string;
  templateHash: string;
  extensionCommitment: string;
  decimals: number;
  /** minimum price step, sompi per whole token (the registry `tick`) */
  tick: string;
  ticker: string;
  name: string;
  /** whole tokens (token2 records it; TUSD's is in the config) */
  supply?: string;
  description?: string;
}

export type GenesisOutputs = Record<string, { transactionId: string; index: number; amount: string; carrier: string }>;

/** Persistent soak state (run/state.json): what setup created. */
export interface SoakState {
  token?: IssuedToken;
  /** genesis token outputs per holder key (the indexer never lists issuance outputs) */
  genesisOutputs?: GenesisOutputs;
  /** the second soak token (config `token2`) and its genesis outputs */
  token2?: IssuedToken;
  genesisOutputs2?: GenesisOutputs;
  /** the third soak token (config `token3`) and its genesis outputs */
  token3?: IssuedToken;
  genesisOutputs3?: GenesisOutputs;
  notes?: string[];
}

const GENESIS_KEY = { token: 'genesisOutputs', token2: 'genesisOutputs2', token3: 'genesisOutputs3' } as const satisfies Record<TokenSlot, keyof SoakState>;

/** the issued token of a slot (state.json) */
export const issuedOf = (state: SoakState, slot: TokenSlot): IssuedToken | undefined => state[slot];
/** the genesis outputs of a slot's issuance */
export const genesisOf = (state: SoakState, slot: TokenSlot): GenesisOutputs | undefined => state[GENESIS_KEY[slot]];
/** records a slot's issuance */
export function setIssued(state: SoakState, slot: TokenSlot, issued: IssuedToken, genesis: GenesisOutputs): void {
  state[slot] = issued;
  state[GENESIS_KEY[slot]] = genesis;
}
/** the slot of an issued soak token, or null */
export function slotOfToken(state: SoakState, covenantId: string): TokenSlot | null {
  for (const s of ['token', 'token2', 'token3'] as const) if (state[s]?.covenantId === covenantId) return s;
  return null;
}

export interface Env {
  cfg: LoadedConfig;
  kob: KobWasm;
  sdk: KaspaSdk;
  node: KaspaRpcNode;
  indexer: HttpIndexer;
  keys: Record<string, KeyEntry>;
  state: SoakState;
  /** the dynamic fee policy (config `fees`) and the oracle over the node's getFeeEstimate (fees.ts) */
  fees: FeeService;
  saveState(): void;
  /** set by the bots process only: while the indexer is not following, BotWallet.submit refuses and the bot loops wait (indexer-gate.ts) */
  gate?: IndexerGate;
}

const req = createRequire(import.meta.url);

export function loadKob(dir: string): KobWasm {
  const raw = req(join(dir, 'kob_wasm.js')) as RawKobWasm;
  const kob = createKob(raw);
  kob.selfCheck();
  return kob;
}

export function loadSdk(dir: string): KaspaSdk {
  return req(join(dir, 'kaspa.js')) as KaspaSdk;
}

export function statePath(cfg: LoadedConfig): string {
  return join(cfg.runPath, 'state.json');
}

export function readState(cfg: LoadedConfig): SoakState {
  const p = statePath(cfg);
  return existsSync(p) ? (JSON.parse(readFileSync(p, 'utf8')) as SoakState) : {};
}

export async function createEnv(cfg: LoadedConfig): Promise<Env> {
  mkdirSync(cfg.runPath, { recursive: true });
  const kob = loadKob(cfg.kobWasmDir);
  const sdk = loadSdk(cfg.kaspaSdkDir);
  const node = new KaspaRpcNode({ sdk, network: cfg.network, url: cfg.nodeUrl, callTimeoutMs: 30_000 });
  await node.connect();
  const indexer = new HttpIndexer({ baseUrl: cfg.indexerUrl, timeoutMs: 15_000 });
  const keys = JSON.parse(readFileSync(join(cfg.runPath, 'keys.json'), 'utf8')) as Record<string, KeyEntry>;
  const state = readState(cfg);
  const env: Env = {
    cfg,
    kob,
    sdk,
    node,
    indexer,
    keys,
    state,
    fees: createFeeService(cfg.feeSettings, node),
    saveState() {
      const p = statePath(cfg);
      writeFileSync(p + '.tmp', JSON.stringify(env.state, null, 2));
      renameSync(p + '.tmp', p);
    },
  };
  return env;
}

export function key(env: Env, name: string): KeyEntry {
  const k = env.keys[name];
  if (!k) throw new Error(`no key named ${name} in run/keys.json (run scripts/keygen.mjs)`);
  return k;
}
