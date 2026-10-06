// Minimal Kaspa cashaddr (bech32-like) codec so the mock node can index UTXOs by script public key and still accept addresses.
// Format: `<prefix>:<base32 of (version byte || payload)><40-bit checksum>`; Schnorr P2PK = version 0 (32 B key),
// ECDSA P2PK = version 1 (33 B), P2SH = version 8 (32 B hash). Verified against the official SDK in test/mock-server.test.ts.
import { fromHex, toHex } from './util.mjs';

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const GEN = [0x98f2bc8e61n, 0x79b76d99e2n, 0xf33e5fb3c4n, 0xae2eabe2a8n, 0x1e4f43e470n];

function polymod(values) {
  let c = 1n;
  for (const d of values) {
    const c0 = c >> 35n;
    c = ((c & 0x07ffffffffn) << 5n) ^ BigInt(d);
    for (let i = 0; i < 5; i++) if ((c0 >> BigInt(i)) & 1n) c ^= GEN[i];
  }
  return c ^ 1n;
}

const prefixValues = (prefix) => [...prefix].map((ch) => ch.charCodeAt(0) & 0x1f);

function convertBits(data, from, to, pad) {
  let acc = 0;
  let bits = 0;
  const out = [];
  const maxv = (1 << to) - 1;
  for (const v of data) {
    acc = (acc << from) | v;
    bits += from;
    while (bits >= to) {
      bits -= to;
      out.push((acc >> bits) & maxv);
    }
    acc &= (1 << bits) - 1;
  }
  if (pad && bits > 0) out.push((acc << (to - bits)) & maxv);
  else if (!pad && (bits >= from || ((acc << (to - bits)) & maxv) !== 0)) throw new Error('invalid padding in address');
  return out;
}

export function encodeAddress(prefix, version, payload) {
  const five = convertBits([version, ...payload], 8, 5, true);
  const checksum = polymod([...prefixValues(prefix), 0, ...five, 0, 0, 0, 0, 0, 0, 0, 0]);
  const cs = [];
  for (let i = 0; i < 8; i++) cs.push(Number((checksum >> BigInt(5 * (7 - i))) & 31n));
  return `${prefix}:${[...five, ...cs].map((v) => CHARSET[v]).join('')}`;
}

/** @returns {{prefix: string, version: number, payload: Buffer}} */
export function decodeAddress(address) {
  const idx = address.indexOf(':');
  if (idx <= 0) throw new Error(`address "${address}" has no prefix`);
  const prefix = address.slice(0, idx);
  const body = address.slice(idx + 1);
  const values = [];
  for (const ch of body) {
    const v = CHARSET.indexOf(ch);
    if (v < 0) throw new Error(`address "${address}" has an invalid character`);
    values.push(v);
  }
  if (values.length < 9) throw new Error(`address "${address}" is too short`);
  if (polymod([...prefixValues(prefix), 0, ...values]) !== 0n) throw new Error(`address "${address}" has a bad checksum`);
  const bytes = convertBits(values.slice(0, -8), 5, 8, false);
  return { prefix, version: bytes[0], payload: Buffer.from(bytes.slice(1)) };
}

// Script public keys use the kaspa string form of the SDK / kob-wasm: version (u16 BE hex) + script hex.
export const p2pkSpk = (pubkeyHex) => `000020${pubkeyHex}ac`;
export const p2shSpk = (hashHex) => `0000aa20${hashHex}87`;

export function spkOfAddress(address) {
  const { version, payload } = decodeAddress(address);
  if (version === 0 && payload.length === 32) return p2pkSpk(toHex(payload));
  if (version === 1 && payload.length === 33) return `000021${toHex(payload)}ab`;
  if (version === 8 && payload.length === 32) return p2shSpk(toHex(payload));
  throw new Error(`unsupported address version ${version} (payload ${payload.length} bytes)`);
}

/** Address of a script public key string, or null when it is not P2PK / P2SH. */
export function addressOfSpk(prefix, spk) {
  let m = /^000020([0-9a-f]{64})ac$/.exec(spk);
  if (m) return encodeAddress(prefix, 0, fromHex(m[1]));
  m = /^000021([0-9a-f]{66})ab$/.exec(spk);
  if (m) return encodeAddress(prefix, 1, fromHex(m[1]));
  m = /^0000aa20([0-9a-f]{64})87$/.exec(spk);
  if (m) return encodeAddress(prefix, 8, fromHex(m[1]));
  return null;
}

export const addressPrefix = (network) => (network === 'mainnet' ? 'kaspa' : network.startsWith('testnet') ? 'kaspatest' : 'kaspadev');
