// Spendable KAS UTXOs of a wallet key, read from the node (never from the indexer: funding must be what the node will accept).
import type { Hex, KeyUtxo } from '../kob/types';
import type { NodeApi, NodeUtxo } from './node';
import { pubkeyToAddress, type KaspaSdk } from './kaspa-sdk';

/** A coinbase output is spendable once its accepting block is this many DAA scores old (consensus coinbase maturity, 100 s at 10 BPS). */
export const COINBASE_MATURITY_DAA = 1000n;

export interface UtxoService {
  /**
   * Plain P2PK KAS UTXOs of `pubkey` that a transaction may spend now: no covenant UTXOs (tokens, orders are managed by
   * their own flows), no immature coinbase. Largest first, ties by outpoint, so results are deterministic.
   */
  fundingFor(pubkey: Hex): Promise<KeyUtxo[]>;
}

export interface UtxoServiceDeps {
  node: NodeApi;
  sdk: KaspaSdk;
  network: string;
  maturityDaa?: bigint;
}

export const p2pkScriptPublicKey = (pubkey: Hex): string => '0000' + '20' + pubkey.toLowerCase() + 'ac';

/** Spendable as plain funding at virtual DAA `daa`? */
export function isSpendableFunding(u: NodeUtxo, pubkey: Hex, daa: bigint | null, maturity: bigint = COINBASE_MATURITY_DAA): boolean {
  if (u.covenantId) return false;
  // an empty script means "not reported" (in-memory mocks); a reported one must be this key's P2PK
  if (u.scriptPublicKey && u.scriptPublicKey.toLowerCase() !== p2pkScriptPublicKey(pubkey)) return false;
  if (u.isCoinbase) {
    if (daa === null) return false;
    if (daa - BigInt(u.blockDaaScore) < maturity) return false;
  }
  return true;
}

export function createUtxoService(deps: UtxoServiceDeps): UtxoService {
  const maturity = deps.maturityDaa ?? COINBASE_MATURITY_DAA;
  return {
    async fundingFor(pubkey: Hex): Promise<KeyUtxo[]> {
      const address = pubkeyToAddress(deps.sdk, pubkey, deps.network);
      const utxos = await deps.node.getUtxosByAddresses([address]);
      // the clock is only needed to judge coinbase maturity: skip the extra round trip when there is none
      const daa = utxos.some((u) => u.isCoinbase && !u.covenantId) ? (await deps.node.getClock()).daa : null;
      return utxos
        .filter((u) => isSpendableFunding(u, pubkey, daa, maturity))
        .map((u): KeyUtxo => ({
          transactionId: u.transactionId,
          index: u.index,
          amount: u.amount,
          blockDaaScore: u.blockDaaScore,
          covenantId: null,
          pubkey: pubkey.toLowerCase(),
        }))
        .sort((a, b) => {
          const d = BigInt(b.amount) - BigInt(a.amount);
          if (d !== 0n) return d > 0n ? 1 : -1;
          return a.transactionId === b.transactionId ? a.index - b.index : a.transactionId < b.transactionId ? -1 : 1;
        });
    },
  };
}
