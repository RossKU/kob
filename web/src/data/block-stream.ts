// The blocks a node announces, as the transactions kob-wasm reads (standalone mode, data/standalone.ts). Two sources give the same shape:
//   * wRPC `block-added` notifications of the official SDK (data/node-rpc.ts): 64-bit values as bigint, script public keys as `version + script` hex
//     strings (or `{ version, script }`), the covenant binding of an output as `{ authorizingInput, covenantId }`, the transaction id in `verboseData`;
//   * the offline mock node's `GET /node/blocks` (data/node-http.ts): the same layout with 64-bit values as decimal strings.
// A node does not send the UTXO entries of the inputs: every input carries an empty entry here (`covenantId: null`, amount 0). That is all kob-wasm
// `recoverOrders` needs (the outpoints, the outputs and their covenant bindings; the covenant id of a genesis is re-derived from the authorising
// outpoint, a continuation can never derive to it), and nothing built from such a transaction is ever signed: every order seen this way is confirmed
// on the node before it is shown (data/standalone.ts).
import type { Hex, TxInputJson, TxJson, TxOutputJson } from '../kob/types';
import { joinSpkString } from './kaspa-sdk';

/** One block as the stream reads it: its transactions (ids known), newest DAA score and time. */
export interface StreamBlock {
  hash: Hex;
  daaScore: bigint;
  /** block header time, ms since the epoch (0 when the source does not say) */
  timestampMs: number;
  transactions: TxJson[];
}

const HEX = /^[0-9a-f]*$/;
const HASH = /^[0-9a-f]{64}$/;

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null;

/** A 64-bit value of either source (bigint, decimal string, safe number) as a decimal string; null when it is none of them. */
function dec(v: unknown): string | null {
  if (typeof v === 'bigint') return v >= 0n ? v.toString() : null;
  if (typeof v === 'number') return Number.isSafeInteger(v) && v >= 0 ? String(v) : null;
  if (typeof v === 'string' && /^[0-9]{1,20}$/.test(v)) return BigInt(v).toString();
  return null;
}

/** A hash of either source (hex string, or an SDK `Hash` with `toString`), lower case; null otherwise. */
function hash(v: unknown): Hex | null {
  const s = typeof v === 'string' ? v : isObj(v) && typeof v.toString === 'function' ? String(v.toString()) : null;
  if (s === null) return null;
  const h = s.toLowerCase();
  return HASH.test(h) ? h : null;
}

const hex = (v: unknown): Hex | null => (typeof v === 'string' && HEX.test(v.toLowerCase()) && v.length % 2 === 0 ? v.toLowerCase() : v === undefined || v === null ? '' : null);

/** A script public key: `version (u16 BE hex) + script hex`, or `{ version, script }`. */
function spk(v: unknown): string | null {
  if (typeof v === 'string') {
    const s = v.toLowerCase();
    return s.length >= 4 && HEX.test(s) && s.length % 2 === 0 ? s : null;
  }
  if (isObj(v) && typeof v.version === 'number' && typeof v.script === 'string' && HEX.test(v.script.toLowerCase())) return joinSpkString(v.version, v.script.toLowerCase());
  return null;
}

const EMPTY_UTXO = { address: null, amount: '0', scriptPublicKey: '', blockDaaScore: '0', isCoinbase: false, covenantId: null } as const;

function input(v: unknown): TxInputJson | null {
  if (!isObj(v)) return null;
  const op = isObj(v.previousOutpoint) ? v.previousOutpoint : v;
  const transactionId = hash(op.transactionId);
  const index = typeof op.index === 'number' && Number.isInteger(op.index) && op.index >= 0 ? op.index : null;
  if (!transactionId || index === null) return null;
  const sig = hex(v.signatureScript);
  return {
    transactionId, index, sequence: dec(v.sequence) ?? '0', sigOpCount: typeof v.sigOpCount === 'number' ? v.sigOpCount : 0,
    computeBudget: typeof v.computeBudget === 'number' ? v.computeBudget : 0, signatureScript: sig ?? '', utxo: { ...EMPTY_UTXO },
  };
}

function output(v: unknown): TxOutputJson | null {
  if (!isObj(v)) return null;
  const value = dec(v.value);
  const script = spk(v.scriptPublicKey);
  if (value === null || script === null) return null;
  let covenant: TxOutputJson['covenant'] = null;
  if (isObj(v.covenant)) {
    const id = hash(v.covenant.covenantId);
    const a = v.covenant.authorizingInput;
    if (!id || typeof a !== 'number' || !Number.isInteger(a) || a < 0) return null;
    covenant = { authorizingInput: a, covenantId: id };
  }
  return { value, scriptPublicKey: script, covenant };
}

/**
 * A transaction of a block notification (either source) as `TxJson`, with empty input UTXO entries; null when it cannot be read whole (an id that
 * is not given, an unreadable input or output: such a transaction is skipped, never half read).
 */
export function txJsonOfBlockTx(v: unknown): TxJson | null {
  if (!isObj(v)) return null;
  const id = hash(isObj(v.verboseData) ? v.verboseData.transactionId : undefined) ?? hash(v.id) ?? hash(v.transactionId);
  if (!id || !Array.isArray(v.inputs) || !Array.isArray(v.outputs)) return null;
  const inputs = v.inputs.map(input);
  const outputs = v.outputs.map(output);
  if (inputs.some((i) => i === null) || outputs.some((o) => o === null)) return null;
  const payload = hex(v.payload);
  const subnetworkId = hex(v.subnetworkId);
  if (payload === null || subnetworkId === null) return null;
  return {
    id, version: typeof v.version === 'number' ? v.version : 0, inputs: inputs as TxInputJson[], outputs: outputs as TxOutputJson[], subnetworkId,
    lockTime: dec(v.lockTime) ?? '0', gas: dec(v.gas) ?? '0', storageMass: dec(v.storageMass) ?? dec(v.mass) ?? '0', payload,
  };
}

/** Coinbase transactions (subnetwork 01) pay miners and never carry a KOB1 record. */
export const isCoinbaseTx = (tx: Pick<TxJson, 'subnetworkId'>): boolean => tx.subnetworkId.startsWith('01');

/** A block notification (`{ header, transactions, verboseData }`, either source) as a `StreamBlock`; null when it has no readable header. */
export function streamBlockOf(v: unknown): StreamBlock | null {
  if (!isObj(v) || !isObj(v.header) || !Array.isArray(v.transactions)) return null;
  const h = v.header;
  const blockHash = hash(h.hash) ?? hash(isObj(v.verboseData) ? v.verboseData.hash : undefined);
  const daa = dec(h.daaScore);
  if (!blockHash || daa === null) return null;
  const ts = dec(h.timestamp);
  const transactions: TxJson[] = [];
  for (const t of v.transactions) {
    const tx = txJsonOfBlockTx(t);
    if (tx && !isCoinbaseTx(tx)) transactions.push(tx);
  }
  return { hash: blockHash, daaScore: BigInt(daa), timestampMs: ts === null ? 0 : Number(ts), transactions };
}
