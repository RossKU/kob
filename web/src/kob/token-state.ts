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
