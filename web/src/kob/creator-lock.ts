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
// The automatic lock (crates/kob-protocol/src/lock_auto.rs, contracts/locks/KobLockAuto.sil) opens
// by itself. The locked states are owned by the P2SH hash of the pinned `KobLockAuto` script of the
// terms (until, key) and the token program, and the genesis also pays a plain 1 KAS coin
// (AUTO_COIN) to that script's address. After the time anyone may send the release, which needs no
// signature: input 0 the coin, input 1 the locked token UTXO (same genesis), output 0 the tokens to
// the key, output 1 the coin's KAS back to the key less at most MAX_TAKE for the fee. The
// declaration names the terms in `lock` (the 44-byte manual template) with `"lock_kind":"auto"`.
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
/**
 * The most KAS the signer takes as a lock coin: LOCK_COIN and a fee margin of 0.1 KAS. A larger
 * coin, or a second one, could not all be spent by the one unlock.
 */
export const LOCK_COIN_MAX = LOCK_COIN + 10_000_000n;
/** The KAS an automatic lock's genesis pays to the lock's address, 1 KAS (kob-protocol `AUTO_COIN`). */
export const AUTO_COIN = LOCK_COIN;
/** The smallest coin a reader takes for an automatic lock, 0.2 KAS (kob-protocol `AUTO_COIN_MIN`). */
export const AUTO_COIN_MIN = 20_000_000n;
/** The most of the coin a release keeps from the creator for its fee and a tip, 0.03 KAS. */
export const MAX_TAKE = 3_000_000n;

/**
 * How a creator lock opens after its time: `manual`, the creator signs the unlock; `auto`, anyone
 * may send the release, which pays only the creator.
 */
export type LockKind = 'manual' | 'auto';
export const isLockKind = (v: unknown): v is LockKind => v === 'manual' || v === 'auto';

/** A creator lock as kob-wasm `creatorLock` returns it. */
export interface CreatorLockInfo {
  /** Unix ms. */
  until: U64;
  key: Hex;
  kind: LockKind;
  /** The 44-byte manual template of the terms, in hex: what the declaration's `lock` holds. */
  terms: Hex;
  /**
   * The redeem script of the locked states' owner, in hex: the terms for a manual lock, the
   * `KobLockAuto` script of the terms and the token program for an automatic one.
   */
  script: Hex;
  /** The p2sh/v1 owner of the locked states, Blake2b(script). */
  authority: Hex;
  /**
   * The script public key of the lock's address, the P2SH of the script. The lock coin goes there.
   */
  scriptPublicKey: string;
}

/** One genesis output a verified lock holds. */
/**
 * One genesis output a verified lock holds: `amount` is base units for a token output and sompi for
 * an automatic lock's coin; `value` is the output's KAS (sompi), from a binding that says it.
 */
export interface LockedOutput { index: number; amount: U64; value?: U64 }

/** A declared creator lock verified against its token's genesis (kob-wasm `kcc23Verify` `lock`). */
export interface VerifiedLock {
  /** Manual (the creator signs) or automatic (anyone releases it to the creator). */
  kind: LockKind;
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
  /**
   * The genesis holds units of another extension class than the locked ones: a share of supply
   * across classes would count units that never trade as the token (R8FX-6).
   */
  otherClass: boolean;
  /** An automatic lock's coin: the genesis's plain KAS output at the lock's address. */
  coin: LockedOutput | null;
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
  buildRelease?(request: string): string;
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

/** How a lock opens and, for an automatic one, the token program of the locked tokens. */
export type LockKindRequest = { kind?: 'manual' } | { kind: 'auto'; program: TokenProgram };

/**
 * A creator lock from its terms or its script (kob-wasm `creatorLock`). It throws KobError for a
 * script that is not exactly the template. With `kind: 'auto'` and the token program it is the
 * automatic lock of those terms: its script, owner and address are `KobLockAuto`'s.
 */
export function creatorLock(kob: KobWasm, l: ({ until: U64 | bigint; key: Hex } | { script: Hex }) & LockKindRequest): CreatorLockInfo {
  const terms = 'script' in l ? { script: l.script } : { until: l.until.toString(), key: l.key };
  const req = l.kind === 'auto' ? { ...terms, kind: 'auto', program: l.program } : terms;
  const out = call<Partial<CreatorLockInfo> & Omit<CreatorLockInfo, 'kind' | 'terms'>>('creatorLock', () => raw(kob).creatorLock(JSON.stringify(req)));
  // a binding before the automatic lock says no kind: its lock is the manual one
  if (l.kind === 'auto' && out.kind !== 'auto') throw new KobError('the loaded kob-wasm bindings do not know the automatic creator lock: rebuild them (npm run build:wasm)', 'creatorLock');
  return { until: out.until, key: out.key, kind: out.kind === 'auto' ? 'auto' : 'manual', terms: out.terms ?? out.script, script: out.script, authority: out.authority, scriptPublicKey: out.scriptPublicKey };
}

/**
 * The creator lock a declaration states for its token program, or null. `lock` is the declared
 * terms (the manual template); `lockKind` `auto` makes it the automatic lock of those terms and
 * `program`, the program the genesis's token outputs are checked under. It never throws.
 */
export function declaredLock(kob: KobWasm, d: { lock: Hex | null; lockKind?: LockKind | null }, program: TokenProgram | null | undefined): CreatorLockInfo | null {
  if (!d.lock) return null;
  if (d.lockKind !== 'auto') return lockOfScript(kob, d.lock);
  if (!program) return null;
  try {
    return creatorLock(kob, { script: d.lock, kind: 'auto', program });
  } catch {
    return null;
  }
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

/** The request of kob-wasm `buildRelease`: an automatic lock's release, which nobody signs. */
export interface ReleaseRequest {
  token: { covenantId: Hex; program: TokenProgram };
  /** The terms. */
  lock: { until: U64; key: Hex };
  /** The locked token UTXO, a genesis output, with its state. */
  tokens: TokenUtxo;
  /** The lock coin, the genesis's plain KAS output at the lock's address. */
  coin: Utxo;
  /** A keeper's own KAS input (signed by it); without one `built.sign` is empty. */
  keeper?: KeyUtxo;
  tip?: U64;
  fee?: FeeOptions;
  ownKeys?: Hex[];
}

/**
 * Builds an automatic lock's release: input 0 the coin (`creatorLockAuto`, no signature), input 1
 * the locked tokens, output 0 the tokens and output 1 the coin's KAS, both to the lock's key. Its
 * lock time is the lock's time.
 */
export function buildRelease(kob: KobWasm, r: ReleaseRequest): BuiltTx {
  const lib = raw(kob);
  if (typeof lib.buildRelease !== 'function') throw new KobError('the loaded kob-wasm bindings do not export the release: rebuild them (npm run build:wasm)', 'buildRelease');
  return call<BuiltTx>('buildRelease', () => lib.buildRelease!(JSON.stringify(r)));
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
  // An older binding names no kind: its lock is manual.
  if (o.kind !== undefined && !isLockKind(o.kind)) return null;
  const kind: LockKind = o.kind === 'auto' ? 'auto' : 'manual';
  let coin: LockedOutput | null = null;
  if (kind === 'manual') {
    // The script must be the template of exactly these terms.
    const terms = parseLockScript(o.script);
    if (!terms || terms.until !== BigInt(o.until) || terms.key !== o.key) return null;
  } else {
    // kob-wasm derived the KobLockAuto script and found the coin: one locked output, one coin
    const c = o.coin as Partial<LockedOutput> | null | undefined;
    if (o.script.length === 0 || !HEX.test(o.script) || outputs.length !== 1 || !c || !Number.isInteger(c.index) || !u64(c.amount) || BigInt(c.amount) < AUTO_COIN_MIN || c.index === outputs[0]!.index) return null;
    coin = { index: c.index!, amount: c.amount, ...(u64(c.value) ? { value: c.value } : {}) };
  }
  return {
    kind, until: o.until, key: o.key, script: o.script, authority: o.authority, amount: o.amount, outputs: outputs.map((x) => ({ index: x.index, amount: x.amount, ...(u64(x.value) ? { value: x.value } : {}) })),
    program: o.program, extensionCommitment: o.extensionCommitment, supply: u64(o.supply) ? o.supply : null, otherClass: o.otherClass === true, coin,
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
/**
 * The lock's share of the supply of the market `marketExt` trades, or null. It is given only when
 * the lock's class is that market's class and the genesis holds no other class (R8FX-6); else the
 * lock is shown without a share.
 */
export function lockShareOfMarket(lock: Pick<VerifiedLock, 'amount' | 'supply' | 'extensionCommitment' | 'otherClass'>, marketExt: Hex | null | undefined): string | null {
  if (lock.otherClass || !marketExt || marketExt.toLowerCase() !== lock.extensionCommitment.toLowerCase()) return null;
  return lockSharePercent(lock.amount, lock.supply);
}

export function lockSharePercent(amount: U64 | bigint, supply: U64 | bigint | null | undefined): string | null {
  if (supply === null || supply === undefined || BigInt(supply) <= 0n) return null;
  const bps = (BigInt(amount) * 10_000n) / BigInt(supply);
  const frac = (bps % 100n).toString().padStart(2, '0').replace(/0+$/, '');
  return `${bps / 100n}${frac ? `.${frac}` : ''}`;
}
