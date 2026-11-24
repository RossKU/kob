// Token states of both families and what the app needs to know about them.
// KCC-20 has a 112-byte state with `owner_scheme`. KRON has a 46-byte state with `id_type`.
// These are pure data helpers over the JSON kob-wasm speaks. kob-wasm decides validity, and the
// builders refuse states they cannot spend.
//
// Ownership in the two layouts:
//   * KCC-20 `owner_scheme` 0 is a key, with the signature inside the token input.
//   * KCC-20 `owner_scheme` 4 is a covenant id, which is KOB custody.
//   * KRON `id_type` 3 is address presence. A P2PK input of the key must be in the transaction.
//     Wallets hold this type and KOB delivers it.
//   * KRON `id_type` 2 is a covenant id, which is KOB custody.
//   * KRON `id_type` 0 is a pubkey with a token-level signature, and 1 is a script hash.
//
// The key-held type of a program's holders is `holderOwner` in its profile. KForge uses 0, so KOB
// delivers to id_type 0. The builders spend that type and refuse the other.
import { CODEC_OF_PROGRAM, TOKEN_PROGRAMS } from './programs.generated';
import type { Family, Hex, KronState, TokenProgram, TokenState, Kcc20State, U64 } from './types';

/** KCC-20 `owner_scheme` of a key-held state. */
export const KCC20_SCHEME_P2PK = 0;
/**
 * KCC-20 `owner_scheme` of a `p2sh/v1` owner, the Blake2b of a redeem script. A creator lock is one
 * (kob/creator-lock.ts).
 */
export const KCC20_SCHEME_P2SH = 3;
/** KCC-20 `owner_scheme` of a state owned by a covenant id. */
export const KCC20_SCHEME_COVID = 4;
/** KRON `id_type` of a pubkey owner with a token-level signature. */
export const KRON_ID_PUBKEY = 0;
/** KRON `id_type` of a script-hash owner. */
export const KRON_ID_SCRIPT_HASH = 1;
/** KRON `id_type` of a state owned by a covenant id. */
export const KRON_ID_COVID = 2;
/** KRON `id_type` of address presence, the default key-held type. */
export const KRON_ID_ADDR = 3;
/** KRON's token program rejects any output above this amount. */
export const KRON_MAX_OUTPUT_AMOUNT = 1_000_000_000n;
const ZERO32 = '00'.repeat(32);

/** True for a KRON state, told apart by its `id_type` or `is_minter` field. */
export const isKronState = (s: TokenState): s is KronState => 'id_type' in s || 'is_minter' in s;
/** True for a KCC-20 state. */
export const isKcc20State = (s: TokenState): s is Kcc20State => !isKronState(s);
/** The token family of a state. */
export const familyOfState = (s: TokenState): Family => (isKronState(s) ? 'kron' : 'kcc20');
/** The token family of an embedded program. */
export const familyOfProgram = (p: TokenProgram): Family => CODEC_OF_PROGRAM[p];

/** The embedded token program with this template hash, or null when there is none. */
export const programOfHash = (hash: Hex | null | undefined): TokenProgram | null =>
  (TOKEN_PROGRAMS.find((p) => p.templateHash === hash?.toLowerCase())?.name as TokenProgram | undefined) ?? null;
/**
 * The owner value of a key-held state of `program`, from its profile `holderOwner`.
 * Without a program it is the family default, 0 for KCC-20 and 3 for KRON.
 */
export const holderOwnerOf = (program: TokenProgram | null | undefined, family: Family): number =>
  TOKEN_PROGRAMS.find((p) => p.name === program)?.holderOwner ?? (family === 'kron' ? KRON_ID_ADDR : KCC20_SCHEME_P2PK);
/**
 * True for a UTXO the wallet key can spend by itself. Builders accept only this kind from a wallet.
 * For KCC-20 that is scheme 0. For KRON it is the holder owner of the program.
 * Without `program` the family default applies.
 */
export const isKeyOwned = (s: TokenState, program?: TokenProgram | null): boolean =>
  isKronState(s) ? s.id_type === holderOwnerOf(program, 'kron') : s.owner_scheme === KCC20_SCHEME_P2PK;
/** True when a covenant id owns the state, which is KOB order custody. */
export const isCovenantOwned = (s: TokenState): boolean => (isKronState(s) ? s.id_type === KRON_ID_COVID : s.owner_scheme === KCC20_SCHEME_COVID);
/**
 * True for KRON tokens held under a key-held type other than the program's holder owner.
 * They show in balances, but the builders cannot spend them.
 */
export const isKronUnspendable = (s: TokenState, program?: TokenProgram | null): boolean =>
  isKronState(s) && s.id_type !== holderOwnerOf(program, 'kron') && s.id_type !== KRON_ID_COVID;
/** True for a KCC-20 state without borrowing or a KRON state without the minter flag. */
export const isPlainState = (s: TokenState): boolean => (isKronState(s) ? s.is_minter === 0 : s.borrow_scheme === 0);
/**
 * True for a state the builders spend from a wallet. That is a plain one, or a key-owned KCC-20
 * holding with a borrow setting, because the owner path checks the owner only, whatever the borrow
 * scheme. Its change keeps the setting. Custodies, deliveries and recipients stay plain.
 */
export const isSpendableHolding = (s: TokenState, program?: TokenProgram | null): boolean =>
  isKeyOwned(s, program) && (isPlainState(s) || isKcc20State(s));
/**
 * The borrow setting of a state as one key, `scheme:guard`. Every plain state has
 * {@link PLAIN_BORROW}.
 */
export const borrowKeyOf = (s: TokenState): string => (isKronState(s) || s.borrow_scheme === 0 ? PLAIN_BORROW : `${s.borrow_scheme}:${s.borrow_guard.toLowerCase()}`);
/** The borrow key of a plain state. */
export const PLAIN_BORROW = `0:${ZERO32}`;
/** The same state with the borrow setting of `from`, for KCC-20. A KRON state is returned as is. */
export const withBorrowOf = <S extends TokenState>(s: S, from: TokenState): S =>
  isKronState(s) || isKronState(from) ? s : ({ ...s, borrow_scheme: from.borrow_scheme, borrow_guard: from.borrow_guard } as S);
/**
 * Groups `utxos` by borrow setting. The plain group comes first, then the others by total, largest
 * first, then by key. Inputs of different settings never share a transaction.
 */
export function groupByBorrow<U extends { state: TokenState }>(utxos: readonly U[]): U[][] {
  const groups = new Map<string, U[]>();
  for (const u of utxos) {
    const k = borrowKeyOf(u.state);
    const g = groups.get(k);
    if (g) g.push(u);
    else groups.set(k, [u]);
  }
  const total = (g: U[]): bigint => g.reduce((s, u) => s + BigInt(u.state.amount), 0n);
  return [...groups.entries()]
    .sort(([ka, a], [kb, b]) => {
      if ((ka === PLAIN_BORROW) !== (kb === PLAIN_BORROW)) return ka === PLAIN_BORROW ? -1 : 1;
      const x = total(a);
      const y = total(b);
      if (x !== y) return x > y ? -1 : 1;
      return ka < kb ? -1 : ka > kb ? 1 : 0;
    })
    .map(([, g]) => g);
}
/** The extension commitment of a KCC-20 state, or null for KRON, which has none. */
export const extensionOfState = (s: TokenState): Hex | null => (isKronState(s) ? null : s.extension_commitment);
/** A short label of the owner type for issue text and tracker listings. */
export const ownerKindOf = (s: TokenState, program?: TokenProgram | null): 'key' | 'covenant' | 'other' =>
  isKeyOwned(s, program) ? 'key' : isCovenantOwned(s) ? 'covenant' : 'other';
/** The numeric owner scheme or id type, whichever the layout has. */
export const ownerTypeOf = (s: TokenState): number => (isKronState(s) ? s.id_type : s.owner_scheme);

/** Builds the state of a token UTXO owned by a wallet key. */
export function keyState(family: Family, amount: U64 | bigint, owner: Hex, extension: Hex | null, program?: TokenProgram | null): TokenState {
  const a = amount.toString();
  return family === 'kron'
    ? { amount: a, owner, id_type: holderOwnerOf(program, 'kron'), is_minter: 0 }
    : { amount: a, owner, owner_scheme: KCC20_SCHEME_P2PK, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: extension ?? ZERO32 };
}
/** Builds the state of an order's custody token UTXO, owned by the order covenant id. */
export function custodyState(family: Family, amount: U64 | bigint, covenantId: Hex, extension: Hex | null): TokenState {
  const a = amount.toString();
  return family === 'kron'
    ? { amount: a, owner: covenantId, id_type: KRON_ID_COVID, is_minter: 0 }
    : { amount: a, owner: covenantId, owner_scheme: KCC20_SCHEME_COVID, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: extension ?? ZERO32 };
}

/**
 * True when a state can share a transfer or order with tokens of extension commitment `ext`.
 * A null `ext` matches anything, and KRON has no extension, so it always matches.
 */
export const extensionMatches = (s: TokenState, ext: Hex | null): boolean => isKronState(s) || ext === null || s.extension_commitment === ext;

/**
 * Whether a token unit is the sell custody of order `owner` as the CONTRACTS accept it (R9 custody
 * comparison, shared with kob-protocol's cancel, position cancel and refund): owned by the order's
 * covenant id (KCC-20 scheme 4, KRON id_type 2), borrow scheme 0, the extension the state pins, the
 * custody's exact amount. The borrow GUARD is ignored: the order covenants' custody checks ignore
 * it, so a unit with a guard is released by a cancel like the exact one and reaches the maker as a
 * plain unit. A bought custody, and the custody a fill spends, must be exact (`custodyState`).
 * `ext` null accepts any class (KRON has none). The token covenant id is the caller's to compare.
 */
export function acceptsAsSellCustody(s: unknown, want: { owner: Hex; amount: bigint; ext: Hex | null }): boolean {
  if (!s || typeof s !== 'object') return false;
  const st = s as Record<string, unknown>;
  if (st.owner !== want.owner || String(st.amount ?? '') !== want.amount.toString()) return false;
  if (isKronState(st as never)) return st.id_type === KRON_ID_COVID;
  if (!isKcc20State(st as never)) return false;
  return st.owner_scheme === KCC20_SCHEME_COVID && Number(st.borrow_scheme) === 0 && (want.ext === null || st.extension_commitment === want.ext);
}
