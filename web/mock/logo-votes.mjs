// The token-logo votes of the mock indexer (`GET` and `POST /v1/logo-votes`, docs/api-reference.md 3.19b; kobd crate::logo_votes).
// It checks what kobd checks of a vote except the store's history rule (every address of the mock has history): the day, the
// proof of work, the Kaspa personal-message signature (BIP340 Schnorr over BLAKE2b-256 keyed `PersonalMessageSigningHash`) by the
// address's key, one vote per wallet per interval across all logos, and an unchanged vote uses nothing. The work is 8 bits, so a
// test casts a vote at once. `POST /mock/logo-votes {covenant_id, up, down}` sets a token's totals (other wallets' votes).
import { createHash } from 'node:crypto';
import { schnorr } from '@noble/curves/secp256k1.js';
import { blake2b } from '@noble/hashes/blake2.js';
import { decodeAddress } from './address.mjs';
import { ApiError, HEX64, badRequest } from './util.mjs';

export const MOCK_POW_BITS = 8;
const DAY_MS = 86_400_000;
const enc = new TextEncoder();

export const voteMessage = (network, id, vote, address, day) => `KOB logo vote\nToken: ${id}\nVote: ${vote}\nAddress: ${address}\nNetwork: ${network}\nDay: ${day}`;
const powInput = (network, id, address, day, nonce) => `KOB logo vote pow/1\n${network}\n${id}\n${address}\n${day}\n${nonce}`;
const zeroBits = (h) => {
  let n = 0;
  for (const b of h) {
    if (b === 0) n += 8;
    else return n + Math.clz32(b) - 24;
  }
  return n;
};
const utcDay = (ms) => new Date(ms).toISOString().slice(0, 10);

export class MockLogoVotes {
  constructor(network = 'testnet-10') {
    this.network = network;
    this.reset();
  }

  reset() {
    /** covenant id -> {up, down} */
    this.totals = new Map();
    /** `${id}|${key}` -> 'up' | 'down' */
    this.current = new Map();
    /** key -> ms of the last vote */
    this.last = new Map();
    this.votes = [];
    this.intervalMs = DAY_MS;
    this.updatedMs = 0;
  }

  seed(b) {
    const id = String(b?.covenant_id ?? '').toLowerCase();
    if (!HEX64.test(id)) throw badRequest('covenant_id must be 64 hex characters');
    this.totals.set(id, { up: Number(b.up ?? 0), down: Number(b.down ?? 0) });
    this.updatedMs = Date.now();
    return { covenant_id: id, ...this.totals.get(id) };
  }

  body() {
    const logos = [...this.totals].filter(([, t]) => t.up + t.down > 0).sort(([a], [b]) => (a < b ? -1 : 1)).map(([covenant_id, t]) => ({ covenant_id, up: t.up, down: t.down }));
    const hidden = logos.filter((t) => t.up + t.down >= 3 && t.up - t.down <= -1).map((t) => t.covenant_id);
    return { logos, hidden, min_voters: 3, hide_at: -1, pow_bits: MOCK_POW_BITS, interval_secs: this.intervalMs / 1000, updated_ms: this.updatedMs };
  }

  cast(b, now = Date.now()) {
    const id = String(b?.covenant_id ?? '').toLowerCase();
    const { vote, address, day, nonce, signature } = b ?? {};
    if (!HEX64.test(id) || (vote !== 'up' && vote !== 'down') || typeof address !== 'string' || typeof day !== 'string' || !/^\d{1,20}$/.test(String(nonce)) || !/^[0-9a-f]{128}$/.test(String(signature))) {
      throw badRequest('the body must be JSON {covenant_id, vote, address, day, nonce, signature}');
    }
    if (day !== utcDay(now) && day !== utcDay(now - DAY_MS)) throw new ApiError(400, 'stale_day', 'the day must be today or yesterday (UTC)');
    let key;
    try {
      const a = decodeAddress(address);
      if (a.prefix !== (this.network === 'mainnet' ? 'kaspa' : 'kaspatest') || a.version !== 0 || a.payload.length !== 32) throw new Error('kind');
      key = a.payload;
    } catch {
      throw new ApiError(400, 'unsupported_address', 'only a Schnorr key address (P2PK) of this network can vote');
    }
    if (zeroBits(createHash('sha256').update(powInput(this.network, id, address, day, nonce)).digest()) < MOCK_POW_BITS) throw new ApiError(400, 'bad_pow', 'the nonce does not carry the work asked for');
    const digest = blake2b(enc.encode(voteMessage(this.network, id, vote, address, day)), { key: enc.encode('PersonalMessageSigningHash'), dkLen: 32 });
    let good = false;
    try {
      good = schnorr.verify(Buffer.from(signature, 'hex'), digest, key);
    } catch {
      good = false;
    }
    if (!good) throw new ApiError(400, 'bad_signature', 'the signature does not verify for this address and message');
    const k = key.toString('hex');
    const pair = `${id}|${k}`;
    const t = this.totals.get(id) ?? { up: 0, down: 0 };
    const before = this.current.get(pair);
    if (before === vote) return { result: 'unchanged', covenant_id: id, up: t.up, down: t.down };
    const last = this.last.get(k);
    if (last !== undefined && now - last < this.intervalMs) throw new ApiError(429, 'too_soon', 'one vote per wallet per 24 hours; try again later');
    if (before) t[before]--;
    t[vote]++;
    this.totals.set(id, t);
    this.current.set(pair, vote);
    this.last.set(k, now);
    this.votes.push({ covenant_id: id, voter: k, vote, day, nonce, at: now });
    this.updatedMs = now;
    return { result: before ? 'changed' : 'counted', covenant_id: id, up: t.up, down: t.down };
  }
}
