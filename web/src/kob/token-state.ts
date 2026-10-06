// Token states of both families (KCC-20: 112-byte state, `owner_scheme`; KRON: 46-byte state, `id_type`) and what the app needs to know about
// them. Pure data helpers over the JSON kob-wasm speaks; kob-wasm decides validity (the builders refuse states they cannot spend).
//
// Ownership in the two layouts:
//   * KCC-20 `owner_scheme` 0 = a key (signature inside the token input), 4 = a covenant id (KOB custody);
//   * KRON `id_type` 3 = address presence (a P2PK input of the key must be in the transaction: what wallets hold and KOB delivers), 2 = covenant
//     id (KOB custody), 0 = pubkey (token-level signature) and 1 = script hash: the builders do not spend those (move them to id_type 3 first).
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
export const familyOfProgram = (p: TokenProgram): Family => (p.startsWith('Kron') ? 'kron' : 'kcc20');

/** A UTXO the wallet key can spend by itself (KCC-20 scheme 0; KRON address presence): the only kind the builders accept from a wallet. */
export const isKeyOwned = (s: TokenState): boolean => (isKronState(s) ? s.id_type === KRON_ID_ADDR : s.owner_scheme === KCC20_SCHEME_P2PK);
/** Owned by a covenant id (KOB order custody). */
export const isCovenantOwned = (s: TokenState): boolean => (isKronState(s) ? s.id_type === KRON_ID_COVID : s.owner_scheme === KCC20_SCHEME_COVID);
/** KRON tokens held by a pubkey / script hash: visible in balances, but not spendable by the builders. */
export const isKronUnspendable = (s: TokenState): boolean => isKronState(s) && s.id_type !== KRON_ID_ADDR && s.id_type !== KRON_ID_COVID;
/** No borrowing (KCC-20) / no minter flag (KRON): the builders refuse anything else. */
export const isPlainState = (s: TokenState): boolean => (isKronState(s) ? s.is_minter === 0 : s.borrow_scheme === 0);
/** The extension commitment of a KCC-20 state; KRON tokens have none (`null`). */
export const extensionOfState = (s: TokenState): Hex | null => (isKronState(s) ? null : s.extension_commitment);
/** Human label of the owner type (issue text, tracker listings). */
export const ownerKindOf = (s: TokenState): 'key' | 'covenant' | 'other' => (isKeyOwned(s) ? 'key' : isCovenantOwned(s) ? 'covenant' : 'other');
/** Numeric owner scheme / id type, whichever the layout has (decoder facts). */
export const ownerTypeOf = (s: TokenState): number => (isKronState(s) ? s.id_type : s.owner_scheme);

/** State of a token UTXO owned by a wallet key. */
export function keyState(family: Family, amount: U64 | bigint, owner: Hex, extension: Hex | null): TokenState {
  const a = amount.toString();
  return family === 'kron'
    ? { amount: a, owner, id_type: KRON_ID_ADDR, is_minter: 0 }
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
