// Token states of both families (KCC-20: 112-byte state, `owner_scheme`; KRON: 46-byte state, `id_type`) and what the app needs to know about
// them. Pure data helpers over the JSON kob-wasm speaks; kob-wasm decides validity (the builders refuse states they cannot spend).
//
// Ownership in the two layouts:
//   * KCC-20 `owner_scheme` 0 = a key (signature inside the token input), 4 = a covenant id (KOB custody);
//   * KRON `id_type` 3 = address presence (a P2PK input of the key must be in the transaction: what wallets hold and KOB delivers), 2 = covenant
//     id (KOB custody), 0 = pubkey (token-level signature) and 1 = script hash. Which key-held type a program's holders are is its profile's
//     `holderOwner` (KForge: 0, the token-level signature; KOB then delivers to id_type 0); the builders spend that one and refuse the other.
import { CODEC_OF_PROGRAM, TOKEN_PROGRAMS } from './programs.generated';
import type { Family, Hex, KronState, TokenProgram, TokenState, Kcc20State, U64 } from './types';

export const KCC20_SCHEME_P2PK = 0;
export const KCC20_SCHEME_COVID = 4;
export const KRON_ID_PUBKEY = 0;
export const KRON_ID_SCRIPT_HASH = 1;
export const KRON_ID_COVID = 2;
export const KRON_ID_ADDR = 3;
/** KRON's token program rejects any output above this amount (registry `KRON_MAX_OUTPUT_AMOUNT`). */
export const KRON_MAX_OUTPUT_AMOUNT = 1_000_000_000n;
const ZERO32 = '00'.repeat(32);

export const isKronState = (s: TokenState): s is KronState => 'id_type' in s || 'is_minter' in s;
export const isKcc20State = (s: TokenState): s is Kcc20State => !isKronState(s);
export const familyOfState = (s: TokenState): Family => (isKronState(s) ? 'kron' : 'kcc20');
export const familyOfProgram = (p: TokenProgram): Family => CODEC_OF_PROGRAM[p];

/** The embedded token program with this template hash (`null`: none). */
export const programOfHash = (hash: Hex | null | undefined): TokenProgram | null =>
  (TOKEN_PROGRAMS.find((p) => p.templateHash === hash?.toLowerCase())?.name as TokenProgram | undefined) ?? null;
/** The owner value of a key-held state of `program` (profile `holderOwner`); the family default without a program (KCC-20 0, KRON 3). */
export const holderOwnerOf = (program: TokenProgram | null | undefined, family: Family): number =>
  TOKEN_PROGRAMS.find((p) => p.name === program)?.holderOwner ?? (family === 'kron' ? KRON_ID_ADDR : KCC20_SCHEME_P2PK);
/**
 * A UTXO the wallet key can spend by itself (KCC-20 scheme 0; KRON: the holder owner of its program, address presence unless the program's holders
 * are id_type 0): the only kind the builders accept from a wallet. Without `program` the family default.
 */
export const isKeyOwned = (s: TokenState, program?: TokenProgram | null): boolean =>
  isKronState(s) ? s.id_type === holderOwnerOf(program, 'kron') : s.owner_scheme === KCC20_SCHEME_P2PK;
/** Owned by a covenant id (KOB order custody). */
export const isCovenantOwned = (s: TokenState): boolean => (isKronState(s) ? s.id_type === KRON_ID_COVID : s.owner_scheme === KCC20_SCHEME_COVID);
/** KRON tokens held by another key-held type than its program's holder owner (or a script hash): visible in balances, not spendable by the builders. */
export const isKronUnspendable = (s: TokenState, program?: TokenProgram | null): boolean =>
  isKronState(s) && s.id_type !== holderOwnerOf(program, 'kron') && s.id_type !== KRON_ID_COVID;
/** No borrowing (KCC-20) / no minter flag (KRON): the builders refuse anything else. */
export const isPlainState = (s: TokenState): boolean => (isKronState(s) ? s.is_minter === 0 : s.borrow_scheme === 0);
/** The extension commitment of a KCC-20 state; KRON tokens have none (`null`). */
export const extensionOfState = (s: TokenState): Hex | null => (isKronState(s) ? null : s.extension_commitment);
/** Human label of the owner type (issue text, tracker listings). */
export const ownerKindOf = (s: TokenState, program?: TokenProgram | null): 'key' | 'covenant' | 'other' =>
  isKeyOwned(s, program) ? 'key' : isCovenantOwned(s) ? 'covenant' : 'other';
/** Numeric owner scheme / id type, whichever the layout has (decoder facts). */
export const ownerTypeOf = (s: TokenState): number => (isKronState(s) ? s.id_type : s.owner_scheme);

/** State of a token UTXO owned by a wallet key (KRON: the holder owner of `program`, address presence without one). */
export function keyState(family: Family, amount: U64 | bigint, owner: Hex, extension: Hex | null, program?: TokenProgram | null): TokenState {
  const a = amount.toString();
  return family === 'kron'
    ? { amount: a, owner, id_type: holderOwnerOf(program, 'kron'), is_minter: 0 }
    : { amount: a, owner, owner_scheme: KCC20_SCHEME_P2PK, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: extension ?? ZERO32 };
}
/** State of an order's custody token UTXO (owned by the order covenant id). */
export function custodyState(family: Family, amount: U64 | bigint, covenantId: Hex, extension: Hex | null): TokenState {
  const a = amount.toString();
  return family === 'kron'
    ? { amount: a, owner: covenantId, id_type: KRON_ID_COVID, is_minter: 0 }
    : { amount: a, owner: covenantId, owner_scheme: KCC20_SCHEME_COVID, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: extension ?? ZERO32 };
}

/** True when a state can share a transfer / order with tokens of extension commitment `ext` (`null` = do not care). KRON has no extension: always true. */
export const extensionMatches = (s: TokenState, ext: Hex | null): boolean => isKronState(s) || ext === null || s.extension_commitment === ext;
