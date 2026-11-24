// The consensus covenant id of a genesis (rusty-kaspa `hashing/covenant_id.rs`), computed from bound
// data alone: the authorising input's outpoint and the outputs bound to the group. The private-key
// frame uses it to tell a true continuation from a genesis bound to an id the app chose (R9FB2-1):
// SIGHASH_ALL does not cover an input's UTXO covenant id, so the app's word on it decides nothing.
//
// covenant_id = blake2b-256 keyed "CovenantID" over
//   txid (32 bytes) | index u32 LE | count u64 LE |
//   per output: index u32 LE | value u64 LE | spk version u16 LE | script length u64 LE | script
// kob-protocol's `verify_genesis` (payload.rs) applies the same hash through the consensus crate;
// tests pin this one to it on real placements.
import { blake2b } from '@noble/hashes/blake2.js';
import type { Hex } from './types';

const hexToBytes = (h: string): Uint8Array => {
  if (h.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(h)) throw new Error('not hex');
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(h.slice(2 * i, 2 * i + 2), 16);
  return out;
};
const toHex = (b: Uint8Array): Hex => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('') as Hex;
const le = (v: bigint, n: number): Uint8Array => {
  const out = new Uint8Array(n);
  let x = v;
  for (let i = 0; i < n; i++) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  return out;
};

export interface GroupOutput { index: number; value: bigint | string | number; scriptPublicKey: string }

/** The genesis covenant id of `outputs` (index, value, script with its 2-byte big-endian version) authorised by `outpoint`. */
export function genesisCovenantId(outpoint: { transactionId: string; index: number }, outputs: readonly GroupOutput[]): Hex {
  const parts: Uint8Array[] = [hexToBytes(outpoint.transactionId), le(BigInt(outpoint.index), 4), le(BigInt(outputs.length), 8)];
  for (const o of outputs) {
    const spk = hexToBytes(o.scriptPublicKey);
    if (spk.length < 2) throw new Error('script public key shorter than its version');
    const version = (spk[0]! << 8) | spk[1]!;
    const script = spk.subarray(2);
    parts.push(le(BigInt(o.index), 4), le(BigInt(o.value), 8), le(BigInt(version), 2), le(BigInt(script.length), 8), script);
  }
  const len = parts.reduce((a, p) => a + p.length, 0);
  const msg = new Uint8Array(len);
  let at = 0;
  for (const p of parts) {
    msg.set(p, at);
    at += p.length;
  }
  return toHex(blake2b(msg, { dkLen: 32, key: new TextEncoder().encode('CovenantID') }));
}

/**
 * True when the outputs bound to `covenantId` and authorised by input `i` of `tx` form a valid
 * GENESIS of that id: on chain they start a new covenant, whatever the app says input `i` carries.
 */
export function isGenesisBinding(tx: { inputs: readonly { transactionId: string; index: number }[]; outputs: readonly { value: unknown; scriptPublicKey: string; covenant?: { authorizingInput: number; covenantId: string } | null }[] }, i: number, covenantId: string): boolean {
  const inp = tx.inputs[i];
  if (!inp) return false;
  const group = tx.outputs
    .map((o, index) => ({ o, index }))
    .filter(({ o }) => o.covenant?.authorizingInput === i && o.covenant.covenantId.toLowerCase() === covenantId.toLowerCase())
    .map(({ o, index }) => ({ index, value: String(o.value), scriptPublicKey: o.scriptPublicKey }));
  if (!group.length) return false;
  try {
    return genesisCovenantId(inp, group) === covenantId.toLowerCase();
  } catch {
    return false;
  }
}
