// The creator lock (crates/kob-protocol/src/creator_lock.rs), a cliff time lock on a share of a
// token's genesis, with no contract change.
//
// The locked token states have owner scheme 0x03 (p2sh/v1) and the owner Blake2b(script), where
// the 44-byte redeem script is
//
//   OP_DATA_8 <until: u64 LE, unix ms> OP_CHECKLOCKTIMEVERIFY OP_DATA_32 <x-only key> OP_CHECKSIG
//
// The token's KCC-23 declaration publishes the script as `lock`. The unlock spends a plain KAS coin
// at the lock's P2SH address together with the locked token UTXOs, with lock time = until. The
// coin is input 0, signed by the key, with a non-final sequence. The node accepts the unlock once
// its past median time passes `until`. Without a coin at that address, the wallet first sends one
// (`buildLockCoin`).
//
// Rust is the one implementation. This module wraps the kob-wasm exports, and parses the template
// for display (`parseLockScript`, which must agree with kob-wasm `creatorLock({script})`).
import { schnorr } from '@noble/curves/secp256k1.js';
import type { BuiltTx, FeeOptions, Hex, KeyUtxo, TokenProgram, TokenUtxo, U64, Utxo } from './types';
import { KobError, type KobWasm } from './wasm';

/**
 * Consensus `LOCK_TIME_THRESHOLD`. A lock time at or above it is unix ms, and below it a DAA score.
 * A creator lock is always a time.
 */
export const LOCK_TIME_THRESHOLD = 500_000_000_000n;
/** The latest `until` taken, 2^53 - 1 ms, which a JavaScript number holds exactly. */
export const MAX_UNTIL = (1n << 53n) - 1n;
/**
 * The farthest ahead a new lock may open: about 10 years (3,653 days). A time in microseconds or a
 * wrong unit would lock the share for millennia.
 */
export const MAX_LOCK_SPAN_MS = 3_653n * 86_400_000n;
/** The redeem script's length in bytes. */
export const LOCK_SCRIPT_LEN = 44;
/**
 * The default KAS of a lock coin, 1 KAS. The unlock pays its fee from it and returns the rest.
 */
export const LOCK_COIN = 100_000_000n;

/** A creator lock as kob-wasm `creatorLock` returns it. */
export interface CreatorLockInfo {
  /** Unix ms. */
  until: U64;
  key: Hex;
  /** The 44-byte redeem script, in hex. */
  script: Hex;
  /** The p2sh/v1 owner of the locked states, Blake2b(script). */
  authority: Hex;
  /**
   * The script public key of the lock's address, the P2SH of the script. The lock coin goes there.
   */
  scriptPublicKey: string;
}

/** One genesis output a verified lock holds. */
export interface LockedOutput { index: number; amount: U64 }

/** A declared creator lock verified against its token's genesis (kob-wasm `kcc23Verify` `lock`). */
export interface VerifiedLock {
  until: U64;
  key: Hex;
  script: Hex;
  authority: Hex;
  /** The base units the genesis locked. */
  amount: U64;
  outputs: LockedOutput[];
  /** The token program the genesis states are proven under. */
  program: TokenProgram;
  extensionCommitment: Hex;
  /** The token's supply when every genesis token output is proven, else null. */
  supply: U64 | null;
}

/** The facts of a template script, its until in unix ms and its key. */
export interface LockTerms { until: bigint; key: Hex }

const HEX = /^(?:[0-9a-f]{2})*$/;
const HEX32 = /^[0-9a-f]{64}$/;

function isCurvePoint(key: Hex): boolean {
  try {
    schnorr.utils.lift_x(BigInt(`0x${key}`));
    return true;
  } catch {
    return false;
  }
}

/**
 * The lock of a redeem script that is exactly the template, else null.
 *
 * None of these is a creator lock:
 * - another opcode, such as CHECKSIGVERIFY or CHECKSEQUENCEVERIFY,
 * - another push, or a byte more or less,
 * - a DAA-score lock time, or an `until` past MAX_UNTIL,
 * - a key off the curve.
 *
 * It agrees with kob-protocol `CreatorLock::parse`. It is a display helper, and kob-wasm always
 * checks what is signed.
 */
export function parseLockScript(script: Hex): LockTerms | null {
  const s = script.toLowerCase();
  if (s.length !== LOCK_SCRIPT_LEN * 2 || !HEX.test(s)) return null;
  const byte = (i: number) => parseInt(s.slice(2 * i, 2 * i + 2), 16);
  if (byte(0) !== 0x08 || byte(9) !== 0xb0 || byte(10) !== 0x20 || byte(43) !== 0xac) return null;
  let until = 0n;
  for (let i = 8; i >= 1; i--) until = (until << 8n) | BigInt(byte(i));
  const key = s.slice(22, 86);
  if (until < LOCK_TIME_THRESHOLD || until > MAX_UNTIL || !isCurvePoint(key)) return null;
  return { until, key };
}

interface RawLock {
  creatorLock(request: string): string;
  buildLockCoin(request: string): string;
  buildUnlock(request: string): string;
}

function raw(kob: KobWasm): RawLock {
  const r = kob.raw as unknown as Partial<RawLock>;
  if (typeof r.creatorLock !== 'function' || typeof r.buildLockCoin !== 'function' || typeof r.buildUnlock !== 'function') {
    throw new KobError('the loaded kob-wasm bindings do not export the creator lock: rebuild them (npm run build:wasm)', 'creatorLock');
  }
  return r as RawLock;
}

function call<T>(what: string, f: () => string): T {
  try {
    return JSON.parse(f()) as T;
  } catch (e) {
    if (e instanceof KobError) throw e;
    throw new KobError(e instanceof Error ? e.message : String(e), what);
  }
}

/**
 * A creator lock from its terms or its script (kob-wasm `creatorLock`). It throws KobError for a
 * script that is not exactly the template.
 */
export function creatorLock(kob: KobWasm, l: { until: U64 | bigint; key: Hex } | { script: Hex }): CreatorLockInfo {
  const req = 'script' in l ? { script: l.script } : { until: l.until.toString(), key: l.key };
  return call<CreatorLockInfo>('creatorLock', () => raw(kob).creatorLock(JSON.stringify(req)));
}

/** The `creatorLock` of a script, or null when it is no creator lock. It never throws. */
export function lockOfScript(kob: KobWasm, script: Hex | null | undefined): CreatorLockInfo | null {
  if (!script) return null;
  try {
    return creatorLock(kob, { script });
  } catch {
    return null;
  }
}

/**
 * The request of kob-wasm `buildLockCoin`, which pays `amount` sompi to the lock's address. The
 * default amount is LOCK_COIN.
 */
export interface LockCoinRequest {
  lock: { until: U64; key: Hex };
  funding: KeyUtxo[];
  amount?: U64;
  change?: Hex;
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/**
 * Builds the transaction of the KAS coin that authorises an unlock. Its output 0 is the coin at the
 * lock's address.
 */
export function buildLockCoin(kob: KobWasm, r: LockCoinRequest): BuiltTx {
  return call<BuiltTx>('buildLockCoin', () => raw(kob).buildLockCoin(JSON.stringify(r)));
}

/** The request of kob-wasm `buildUnlock`. */
export interface UnlockRequest {
  token: { covenantId: Hex; program: TokenProgram };
  lock: { until: U64; key: Hex };
  /** The token UTXOs the lock holds. */
  tokens: TokenUtxo[];
  /** The lock coin, a plain KAS UTXO at the lock's address. */
  coin: Utxo;
  /** The tokens' receiving key. The default is the lock's key. */
  to?: Hex;
  carrier?: U64;
  funding?: KeyUtxo[];
  change?: Hex;
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/**
 * Builds a creator lock's unlock, one transaction with lock time = until. Its plans[0] is the coin
 * (`creatorLock`).
 */
export function buildUnlock(kob: KobWasm, r: UnlockRequest): BuiltTx {
  return call<BuiltTx>('buildUnlock', () => raw(kob).buildUnlock(JSON.stringify(r)));
}

const u64 = (v: unknown): v is string => typeof v === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(v);

/**
 * The verified lock of a `kcc23Verify` result, checked again for shape, or null. A binding of
 * another build must not put other types in the UI. `isProgram` tells a token program name.
 */
export function verifiedLockOf(v: unknown, isProgram: (name: string) => name is TokenProgram): VerifiedLock | null {
  if (!v || typeof v !== 'object') return null;
  const o = v as Record<string, unknown>;
  const outputs = Array.isArray(o.outputs)
    ? o.outputs.filter((x): x is LockedOutput => !!x && typeof x === 'object' && Number.isInteger((x as LockedOutput).index) && u64((x as LockedOutput).amount))
    : [];
  if (!u64(o.until) || typeof o.key !== 'string' || !HEX32.test(o.key) || typeof o.script !== 'string' || typeof o.authority !== 'string' || !HEX32.test(o.authority)) return null;
  if (!u64(o.amount) || outputs.length === 0 || typeof o.program !== 'string' || !isProgram(o.program) || typeof o.extensionCommitment !== 'string' || !HEX32.test(o.extensionCommitment)) return null;
  // The script must be the template of exactly these terms.
  const terms = parseLockScript(o.script);
  if (!terms || terms.until !== BigInt(o.until) || terms.key !== o.key) return null;
  return {
    until: o.until, key: o.key, script: o.script, authority: o.authority, amount: o.amount, outputs: outputs.map((x) => ({ index: x.index, amount: x.amount })),
    program: o.program, extensionCommitment: o.extensionCommitment, supply: u64(o.supply) ? o.supply : null,
  };
}

/**
 * Tells whether the lock is open by the wall clock `now` in ms. `marginMs` allows for the node's
 * past median time, which lags it.
 */
export const lockOpen = (until: U64 | bigint, now: number = Date.now(), marginMs = 0): boolean => BigInt(until) + BigInt(marginMs) <= BigInt(Math.floor(now));

/**
 * The share of the supply a lock holds, as a percent with at most 2 decimals, such as `40` or
 * `12.5`. It is null without a known positive supply.
 */
export function lockSharePercent(amount: U64 | bigint, supply: U64 | bigint | null | undefined): string | null {
  if (supply === null || supply === undefined || BigInt(supply) <= 0n) return null;
  const bps = (BigInt(amount) * 10_000n) / BigInt(supply);
  const frac = (bps % 100n).toString().padStart(2, '0').replace(/0+$/, '');
  return `${bps / 100n}${frac ? `.${frac}` : ''}`;
}
