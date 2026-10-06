// What a chain observer (the real indexer, here the mock) reads out of signature scripts. This is observation of on-chain data, not
// protocol logic: the indexer does the same in crates/kob-executor/src/indexer/record.rs (`parse_reveal`, `leader_next_states`).
import { fromHex } from './util.mjs';

/** Data pushes of a signature script (`script.rs::parse_pushes`), or null when it holds anything but pushes. */
export function parsePushes(scriptHex) {
  const b = fromHex(scriptHex);
  const out = [];
  let i = 0;
  while (i < b.length) {
    const op = b[i++];
    let len;
    if (op === 0x00) {
      out.push(Buffer.alloc(0));
      continue;
    }
    if (op <= 0x4b) len = op;
    else if (op === 0x4c) {
      if (i >= b.length) return null;
      len = b[i++];
    } else if (op === 0x4d) {
      if (i + 2 > b.length) return null;
      len = b.readUInt16LE(i);
      i += 2;
    } else if (op === 0x4e) {
      if (i + 4 > b.length) return null;
      len = b.readUInt32LE(i);
      i += 4;
    } else if (op === 0x4f) {
      out.push(Buffer.from([0x81]));
      continue;
    } else if (op >= 0x51 && op <= 0x60) {
      out.push(Buffer.from([op - 0x50]));
      continue;
    } else return null;
    if (i + len > b.length) return null;
    out.push(b.subarray(i, i + len));
    i += len;
  }
  return out;
}

/** The dispatch tag (hex) and redeem script of a P2SH input, or null. */
export function revealOf(scriptHex) {
  const p = parsePushes(scriptHex);
  if (!p || p.length < 2) return null;
  const tag = p[p.length - 2];
  if (tag.length !== 4) return null;
  return { tag: tag.toString('hex'), redeem: p[p.length - 1].toString('hex'), pushes: p };
}

/**
 * The output states a KCC-20 `transfer` (leader) call authorises, from its signature script: the `State[]` argument is encoded by
 * field (amounts 8 B LE, owners 32 B, owner schemes 1 B, borrow schemes 1 B, borrow guards 32 B, extension commitments 32 B), then
 * the witness, the dispatch tag and the redeem script. Returns null for any other input.
 * @returns {import('../src/kob/types').Kcc20State[] | null}
 */
export function leaderNextStates(scriptHex, transferTagHex) {
  const p = parsePushes(scriptHex);
  if (!p || p.length !== 9) return null;
  if (p[7].length !== 4 || p[7].toString('hex') !== transferTagHex) return null;
  const n = p[0].length / 8;
  if (!Number.isInteger(n) || p[1].length !== 32 * n || p[2].length !== n || p[3].length !== n || p[4].length !== 32 * n || p[5].length !== 32 * n) {
    return null;
  }
  const states = [];
  for (let k = 0; k < n; k++) {
    states.push({
      amount: p[0].readBigInt64LE(8 * k).toString(),
      owner: p[1].subarray(32 * k, 32 * k + 32).toString('hex'),
      owner_scheme: p[2][k],
      borrow_scheme: p[3][k],
      borrow_guard: p[4].subarray(32 * k, 32 * k + 32).toString('hex'),
      extension_commitment: p[5].subarray(32 * k, 32 * k + 32).toString('hex'),
    });
  }
  return states;
}
