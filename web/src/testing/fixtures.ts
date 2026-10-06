// Shared test fixtures of the order-planning layer: mock TokenMarkets (3x3 reference program and the 8x8 KOB program), a PlanEnv with
// funding / token UTXOs / a book / a clock, and `consensusCheck` (sign locally -> finalize -> script-engine validate), the strongest
// oracle for a planned transaction. Stable API: other work items import it.
import { loadKobNode } from '../kob/wasm.node';
import type { KobWasm } from '../kob/wasm';
import type { BookLevel, BookOrder, BookView, Clock, OwnOrderRef, PlanEnv, TokenMarket } from '../kob/plan-types';
import type { BuiltTx, Hex, KeyUtxo, SignedTx, TokenUtxo } from '../kob/types';
import { toTokenMarket, type RegistryTokenLike } from '../kob/token-market';
import { keyState } from '../kob/token-state';
import { pubkeyOf, signBuilt } from './local-signer';

export const kob = (): KobWasm => loadKobNode();

/** Deterministic test keys (32-byte secrets, hex) and the x-only public key of the first one (the maker). */
export const MAKER_SK: Hex = '11'.repeat(32);
export const OTHER_SK: Hex = '22'.repeat(32);
export const MAKER_PK: Hex = pubkeyOf(MAKER_SK);
export const OTHER_PK: Hex = pubkeyOf(OTHER_SK);

/** Golden-vector token identity: covenant id 70..70, extension commitment ee..ee. */
export const TOKEN_COV: Hex = '70'.repeat(32);
export const TOKEN_EXT: Hex = 'ee'.repeat(32);

export const KAS = 100_000_000n;

/** Registry-shaped token entry for the test tokens (3 decimals: scale 1000 base units = 1 token, tick 100 sompi per token). */
export function registryToken(template_id: 'kcc20-ref-3x3' | 'kcc20-ref-8x8' | 'kron-2433' | 'kron-2732', over: Partial<RegistryTokenLike> = {}): RegistryTokenLike {
  const kron = template_id.startsWith('kron');
  return {
    ticker: kron ? 'TSTK' : template_id === 'kcc20-ref-3x3' ? 'TST3' : 'TST8',
    covenant_id: TOKEN_COV,
    template_id,
    extension_commitment: kron ? null : TOKEN_EXT,
    decimals: 3,
    tick: 100,
    family: kron ? 'kron' : 'kcc20',
    ...over,
  };
}

/** TokenMarket of the reference 3/3 program (KCC20Ref: prefix 1, suffix 2977). */
export const market3x3 = (over: Partial<RegistryTokenLike> = {}): TokenMarket => toTokenMarket(kob(), registryToken('kcc20-ref-3x3', over));
/** TokenMarket of the 8/8 program KOB issues (KCC20Ref_8x8). */
export const market8x8 = (over: Partial<RegistryTokenLike> = {}): TokenMarket => toTokenMarket(kob(), registryToken('kcc20-ref-8x8', over));

/** TokenMarket of the common KRON program (KronToken2433, 46-byte state, no extension commitment). */
export const marketKron = (over: Partial<RegistryTokenLike> = {}): TokenMarket => toTokenMarket(kob(), registryToken('kron-2433', over));

let seq = 1;
const txid = (): Hex => (seq++).toString(16).padStart(64, '0');

/** A plain P2PK KAS UTXO of `pubkey`. */
export function keyUtxo(amountSompi: bigint, pubkey: Hex = MAKER_PK, blockDaaScore = 500n): KeyUtxo {
  return { transactionId: txid(), index: 0, amount: amountSompi.toString(), blockDaaScore: blockDaaScore.toString(), covenantId: null, pubkey };
}

/** A key-owned token UTXO (KCC-20 P2PK scheme; KRON address presence) of `market` holding `baseUnits` and carrying `kasSompi` KAS. */
export function tokenUtxo(market: TokenMarket, baseUnits: bigint, kasSompi: bigint = 10n * KAS, owner: Hex = MAKER_PK): TokenUtxo {
  return {
    transactionId: txid(),
    index: 1,
    amount: kasSompi.toString(),
    blockDaaScore: '500',
    covenantId: market.covenantId,
    state: keyState(market.family, baseUnits, owner, market.extensionCommitment),
  };
}

/** 15:00:00 UTC on a fixed day, DAA 1,000,000, nominal rate (the golden day-order vector: deadline 1_790_726_400). */
export const CLOCK: Clock = { daa: 1_000_000n, unixSeconds: 1_790_694_000n, rateMilli: 10_000 };

/** One whole test token: 1000 base units (3 decimals). */
export const TOK = 1000n;

/** A book level: `price` sompi per whole token, `amount` base units. */
export const level = (price: bigint, amount: bigint, orders = 1, tip?: bigint): BookLevel => (tip === undefined ? { price, amount, orders } : { price, amount, orders, tip });
export const bookOrder = (price: bigint, amount: bigint, tip?: bigint, minFill?: bigint): BookOrder => {
  const o: BookOrder = { price, amount };
  if (tip !== undefined) o.tip = tip;
  if (minFill !== undefined) o.minFill = minFill;
  return o;
};

/** Aggregated book: best ask 2.50 KAS per token, best bid 2.45 KAS per token. */
export function defaultBook(): BookView {
  return {
    asks: [level(250_000_000n, 5n * TOK, 2), level(252_000_000n, 10n * TOK, 3), level(260_000_000n, 20n * TOK, 4)],
    bids: [level(245_000_000n, 5n * TOK, 1), level(243_000_000n, 10n * TOK, 2), level(240_000_000n, 20n * TOK, 3)],
  };
}

export interface EnvOptions {
  market?: TokenMarket;
  /** KAS funding UTXOs (sompi); default one of 1,000 KAS */
  funding?: bigint[];
  /** token UTXO amounts in base units; default one of 100 tokens (100_000 base units) */
  tokenAmounts?: bigint[];
  book?: BookView;
  ownOrders?: OwnOrderRef[];
  clock?: Clock;
  feeRate?: bigint;
  carrier?: bigint;
}

/** A PlanEnv for `MAKER_PK`. The wasm module is the real one (node bindings). */
export function makeEnv(o: EnvOptions = {}): PlanEnv {
  const market = o.market ?? market3x3();
  const env: PlanEnv = {
    kob: kob(),
    token: market,
    maker: MAKER_PK,
    clock: o.clock ?? CLOCK,
    book: o.book ?? defaultBook(),
    ownOrders: o.ownOrders ?? [],
    funding: (o.funding ?? [1000n * KAS]).map((a) => keyUtxo(a)),
    tokenUtxos: (o.tokenAmounts ?? [100n * TOK]).map((a) => tokenUtxo(market, a)),
  };
  if (o.feeRate !== undefined) env.feeRate = o.feeRate;
  if (o.carrier !== undefined) env.carrier = o.carrier;
  return env;
}

/** Signs a built transaction with the given secret keys (default: the maker), finalizes with tightened budgets and runs the script engine. */
export function consensusCheck(k: KobWasm, built: BuiltTx, keys: Hex[] = [MAKER_SK]): SignedTx {
  const sigs = signBuilt(built, keys);
  const signed = k.finalize(built, sigs, { tightenBudgets: true });
  k.validate(signed);
  return signed;
}
