// Local Schnorr signing for tests, the mock wallet and e2e harnesses (NOT used by the production app: the app never sees a key).
import { schnorr } from '@noble/curves/secp256k1.js';
import type { BuiltTx, Hex, InputSignature } from '../kob/types';

export const hexToBytes = (h: string): Uint8Array => Uint8Array.from((h.match(/../g) ?? []).map((b) => parseInt(b, 16)));
export const bytesToHex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

/** x-only public key (hex) of a 32-byte secret key (hex). */
export const pubkeyOf = (sk: Hex): Hex => bytesToHex(schnorr.getPublicKey(hexToBytes(sk)));

/** SIGHASH_ALL Schnorr signature over a digest: 65 bytes (sig64 + 0x01), hex. */
export function signDigest(sk: Hex, digest: Hex): Hex {
  return bytesToHex(schnorr.sign(hexToBytes(digest), hexToBytes(sk))) + '01';
}

/** Signs every `built.sign` request whose pubkey matches one of `keys` (secret keys, hex). Throws if a request has no key. */
export function signBuilt(built: BuiltTx, keys: Hex[]): InputSignature[] {
  const byPub = new Map(keys.map((k) => [pubkeyOf(k), k]));
  return built.sign.map((r) => {
    const sk = byPub.get(r.pubkey);
    if (!sk) throw new Error(`no test key for ${r.pubkey} (input ${r.inputIndex})`);
    return { inputIndex: r.inputIndex, signature: signDigest(sk, r.sighash) };
  });
}
