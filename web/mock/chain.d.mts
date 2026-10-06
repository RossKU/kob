// Type declarations of the mock chain (implementation: chain.mjs). Only the surface tests and fixtures use is typed; records are loose.
export const DEFAULT_CARRIER: bigint;

export interface MockChain {
  network: string;
  settleDepthDaa: number;
  blockSeq: number;
  /** covenant id -> stored order record */
  orders: Map<string, any>;
  tokens: Map<string, any>;
  tokenUtxos: any[];
  events: any[];
  rejects: { txid: string; reason: string }[];
  /** accepted submissions with a decoded summary (`created`, `closed`, `token_outputs`, `records`, `tx`) */
  submissions: any[];
  listeners: Set<(ev: any) => void>;
  daa(): number;
  nowUnix(): number;
  advanceDaa(n: number): number;
  reset(): void;
  giveKas(pubkey: string, sompi: bigint | string | number): { transactionId: string; index: number; amount: string };
  giveTokens(pubkey: string, token: string, amount: bigint | string | number, carrier?: bigint): { transactionId: string; index: number; token: string; amount: string; value: string };
  kasBalance(pubkey: string): bigint;
  tokenBalance(pubkey: string, token: string): bigint;
  submit(tx: unknown): { transactionId: string };
  /**
   * `amount` base units (default: one whole token or the minimum fill if larger, at most all left), `price` sompi per whole token. A pair order is
   * filled by a real batch: `via` (inventory | netting | route), `against` (netting), `leg` (a conditional's leg), `mode` (evidence 0 | 1).
   */
  simulateFill(covenantId: string, opts?: { amount?: bigint | number | string; price?: string | number | bigint; taker?: string; via?: 'inventory' | 'netting' | 'route'; against?: string; leg?: 0 | 1; mode?: 0 | 1; merge?: boolean }): any;
  seedOrder(any: unknown, opts?: { value?: bigint; custodyCarrier?: bigint; covenantId?: string; deadline?: number | null; parent?: string | null; daa?: number }): { covenantId: string; [k: string]: any };
  seedStray(covenantId: string, amount: bigint | string | number, carrier?: bigint, token?: string | { covenant_id: string; program?: string; ticker?: string; decimals?: number } | null): any;
  /** a pair stop: armed (or, `trail`, trailed) by a real update batch next to evidence of `mode` 0 | 1 */
  armOrder(covenantId: string, opts?: { mode?: 0 | 1; trail?: boolean }): any;
  /** pair fills (volume only): `pair_fills` rows */
  pairFills: any[];
  kob: any;
  liveTokenUtxos(owner: string, role: string): any[];
}
