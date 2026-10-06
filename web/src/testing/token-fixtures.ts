// Test fixtures of the data layer: synthetic keys, KAS / token UTXOs and a `sendTokens` request, so tests can run REAL kob-wasm builds
// (consensus-valid once signed with `local-signer`) against the in-memory `MockNode`.
import type { Hex, KeyUtxo, Kcc20State, SendTokensRequest, TokenProgram, TokenUtxo } from '../kob/types';
import { pubkeyOf } from './local-signer';

export const MAKER_SK: Hex = '0101010101010101010101010101010101010101010101010101010101010101';
export const OTHER_SK: Hex = '0202020202020202020202020202020202020202020202020202020202020202';
export const MAKER_PK: Hex = pubkeyOf(MAKER_SK);
export const OTHER_PK: Hex = pubkeyOf(OTHER_SK);

export const TOKEN_COV_ID: Hex = '70'.repeat(32);
export const TOKEN_EXT: Hex = 'ee'.repeat(32);
export const TOKEN_PROGRAM: TokenProgram = 'KCC20Ref';
export const CARRIER = '1000000000'; // 10 KAS

export const tokenState = (amount: bigint | string, owner: Hex = MAKER_PK, ext: Hex = TOKEN_EXT): Kcc20State => ({
  amount: amount.toString(), owner, owner_scheme: 0, borrow_scheme: 0, borrow_guard: '00'.repeat(32), extension_commitment: ext,
});

const txid = (n: number): Hex => n.toString(16).padStart(2, '0').repeat(32);

/** A P2PK-owned token UTXO with a synthetic outpoint (`txid` = the byte `n` repeated). */
export function tokenUtxo(n: number, amount: bigint | string, index = 1, owner: Hex = MAKER_PK): TokenUtxo {
  return {
    transactionId: txid(n), index, amount: CARRIER, blockDaaScore: '500', covenantId: TOKEN_COV_ID, state: tokenState(amount, owner),
  };
}

export function keyUtxo(n: number, sompi: bigint | string = '5000000000', index = 0, pubkey: Hex = MAKER_PK): KeyUtxo {
  return { transactionId: txid(n), index, amount: sompi.toString(), blockDaaScore: '500', covenantId: null, pubkey };
}

export interface SendOpts {
  tokens?: TokenUtxo[];
  recipient?: Hex;
  amount?: bigint | string;
  funding?: KeyUtxo[];
  change?: Hex | null;
}

/** Sends `amount` of the token from `tokens` to `recipient`; the remainder returns to the maker as a token change output. */
export function sendTokensRequest(o: SendOpts = {}): SendTokensRequest {
  return {
    action: 'sendTokens',
    token: { covenantId: TOKEN_COV_ID, program: TOKEN_PROGRAM },
    tokens: o.tokens ?? [tokenUtxo(1, 7000n), tokenUtxo(2, 5000n, 2)],
    recipients: [{ pubkey: o.recipient ?? OTHER_PK, amount: (o.amount ?? 9000n).toString(), carrier: CARRIER }],
    tokenChange: MAKER_PK,
    tokenChangeCarrier: CARRIER,
    funding: o.funding ?? [keyUtxo(3)],
    change: o.change === undefined ? MAKER_PK : o.change,
    fee: { feeRate: null },
  };
}
