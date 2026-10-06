// PRE-SIGN CONFIRMATION MODEL (safety-critical).
//
// Wallet popups are blind to token semantics (KasWare "Sign Transaction: Spend", Kaspire "covenant, cannot verify", Kastle misleading balance
// changes), so this module is the user's only real check of what a signature authorises. `decodeSigning` re-derives EVERYTHING from the
// built transaction (`built.tx` + `built.plans` + `built.sign`) and kob-wasm (`recoverOrders`, `decodePayload`, `decodeState`,
// `scriptPublicKey`, `tokenScriptPublicKey`): never from planner or form state. Planner claims can be passed as `expected` and are only
// COMPARED against the derived facts; any disagreement is a blocking finding.
//
// Output is plain data with stable `code`s and English `message`s (+ `params`) so the UI can translate. `blocking` findings mean the UI must
// refuse to sign. What it checks, in short:
//   * every input's script equals the script derived from its plan (order state / token state / P2PK key): a tx cannot claim to spend one
//     thing and carry another;
//   * every output is classified by RE-DERIVING its script public key (new order covenant, token custody, token change, KAS change, payment out,
//     unknown); anything leaving the maker beyond planned orders / fee is blocking (payment or token transfer to another key), unless the planner
//     listed it in `expected.transfers` / `expected.payments`;
//   * placement records are re-verified (`recoverOrders` + spk re-derivation), order makers must be the wallet key, KAS and tokens must balance,
//     the fee is recomputed from inputs minus outputs and bounded;
//   * an in-place amend (AMEND record, `recoverAmends`) or sweep (SWEEP record, sweep.ts `verify_sweep` re-check: the SAME script continues the
//     order input's covenant id) is the only way an order's covenant id may continue: any other continuation, or a record that does not verify,
//     is blocking; a sweep's token outputs are its strays returned to the maker (anything to another key is blocking, as everywhere);
//   * a conditional order or stop entry armed (or trailed) by the tx (a fill of its unarmed stop leg / stop entry, or `update`) shows its
//     trigger evidence (touch): the input its `ev` argument names (and, for an ask, its custody `tk`), re-derived from that input's plan and
//     checked against the order's rule (plain KobAsk / KobBid of the same token and scale, filled here, side, quote vs the stop, base units filled
//     vs `minTouch`, exposure vs `minRestDaa`); evidence that is not a plain order of the token, or fails the rule, is blocking;
//   * pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`): both tokens are checked against the registry, a new order's terms against kob-wasm
//     `checkNewOrder`, its custodies (one per token: a sell-first entry's A and its B prefund) against kob-wasm `custodies`; a spend reports
//     what returns of A and of B (custodies and strays of both tokens); a pair stop armed / trailed by the tx shows its evidence in either mode
//     (two KAS-book fills of A and B: the implied rate; or the fill of a resting `KobPair` of the same pair), checked with kob-wasm `pairArms` /
//     `condPairTrailCheck` and the exposure / size / side rules of kob-wasm `pairTriggerRule`. Nothing of a pair order is a KAS price.
// The Schnorr digests in `built.sign` are produced by kob-wasm; TypeScript cannot recompute them, `kob.finalize` verifies signatures against them.
import type { WalletId } from '../wallet/types';
import {
  baseKind, custodiesOf, describeLegacy, describeOrder, familyOfKind, formatKas, formatUnits, isOrderKind, isPairKind, kindFor, big, pairFactsOf, type OrderDescription,
  type PairTokenFacts,
} from './order-facts';
import { declaredRateLimit, type FeePolicy } from './fee-policy';
import { displayName, type TokenRegistry } from './registry';
import type {
  BuiltTx, FeeMode, Hex, LegacyState, OrderKind, OrderState, PairEvidence, PairTriggerRule, Payload, PayloadRecord, RecoveredAmend, RecoveredOrder, RetiredKind, SigArg, SigPlan, TemplateName,
  TokenProgram, TokenState, TxInputJson, TxOutputJson,
} from './types';
import type { NodeInputFacts } from './node-verify';
import { outpointKey } from './node-verify';
import { extensionOfState, isCovenantOwned, isKeyOwned, ownerTypeOf } from './token-state';
import { recoverSweeps, type RecoveredSweep } from './sweep';
import type { KobWasm } from './wasm';

// ------------------------------------------------------------------------------------------------ constants

const ONE_KAS = 100_000_000n;
/** Below this a fee is never "excessive" whatever the amounts (a tiny cancel legitimately costs ~1% of a 1 KAS bid). */
export const FEE_FLOOR_OK = ONE_KAS / 10n;
/** fee > FEE_FLOOR_OK and > 1% of the KAS the tx moves (locked, released, paid out) is blocking; huge trades may pay more than 1 KAS. */
export const FEE_MAX_PERCENT = 1n;
/**
 * Absolute fee ceiling, whatever the amounts: a builder (or a hostile input value) cannot skim more than this as "fee". The dearest golden
 * transaction (an 8x8 batch match) pays 0.104 KAS and every web-built cancel / order less than 0.04 KAS at the relay floor. In `priority`
 * (storage-inclusive) fee mode the ceiling is three times the builder's own minimum when that is higher.
 */
export const FEE_ABSOLUTE_MAX = ONE_KAS / 2n;
/** Warning when the fee is more than this multiple of the builder's own minimum (+ slack). */
export const FEE_WARN_FACTOR = 2n;
export const FEE_WARN_SLACK = 100_000n;

// ------------------------------------------------------------------------------------------------ issue model

export const SIGNING_ISSUE_CODES = [
  // blocking
  'malformed-built', 'recover-failed', 'order-spk-mismatch', 'order-not-maker', 'order-unrecorded', 'order-token-mismatch', 'input-script-mismatch', 'output-script-mismatch',
  'token-output-count', 'token-unbalanced', 'kas-unbalanced', 'fee-mismatch', 'fee-excessive', 'fee-rate-excessive', 'payment-out', 'transfer-out', 'token-to-unknown-owner',
  'output-unknown', 'sign-foreign-key', 'sign-mismatch', 'sign-unexpected', 'sighash-type', 'expected-orders', 'expected-kas-locked',
  'expected-tokens-escrowed', 'expected-cancel-ids', 'expected-max-fee', 'input-unconfirmed', 'token-state-unplain', 'pair-token-mismatch', 'pair-family-mismatch',
  'pair-terms-invalid', 'expected-quote-escrowed', 'trigger-evidence-invalid', 'trigger-rule-unmet', 'retired-not-cancel', 'sweep-invalid', 'expected-sweep-ids', 'sweep-custody-spent',
  // warnings
  'retired-cancel', 'inputs-unverified', 'tx-version', 'fee-high', 'unsigned-maker-input', 'foreign-input', 'payload-unknown', 'payload-invalid', 'token-unlisted', 'no-order-records',
  // info
  'payment-expected', 'transfer-expected', 'strays-swept', 'other-strays-swept', 'kas-released', 'trigger-evidence', 'swept-in-place',
] as const;
export type SigningIssueCode = (typeof SIGNING_ISSUE_CODES)[number];
export type IssueLevel = 'blocking' | 'warning' | 'info';

const LEVEL: Record<SigningIssueCode, IssueLevel> = Object.fromEntries(
  SIGNING_ISSUE_CODES.map((c) => [
    c,
    (['retired-cancel', 'inputs-unverified', 'tx-version', 'fee-high', 'unsigned-maker-input', 'foreign-input', 'payload-unknown', 'payload-invalid', 'token-unlisted', 'no-order-records'] as string[]).includes(c)
      ? 'warning'
      : (['payment-expected', 'transfer-expected', 'strays-swept', 'other-strays-swept', 'kas-released', 'trigger-evidence', 'swept-in-place'] as string[]).includes(c)
        ? 'info'
        : 'blocking',
  ]),
) as Record<SigningIssueCode, IssueLevel>;

export interface SigningIssue {
  code: SigningIssueCode;
  severity: IssueLevel;
  message: string;
  params?: Record<string, string | number | bigint>;
  input?: number;
  output?: number;
}

// ------------------------------------------------------------------------------------------------ model types

export interface TokenRef {
  covenantId: Hex;
  ticker: string | null;
  decimals: number | null;
  /** `TICKER (abcd...1234) [verified]` for registry tokens, `unknown token (abcd...1234)` otherwise: never a bare name */
  display: string;
  inRegistry: boolean;
  tradable: boolean;
}

export interface DecodedInput {
  index: number;
  role: string;
  outpoint: { txid: Hex; index: number };
  type: 'kas' | 'token' | 'order' | 'unknown';
  /** KAS on the input UTXO, sompi */
  amount: bigint;
  /** the wallet will be asked for a signature over this input (`built.sign`) */
  willSign: boolean;
  signer: Hex | null;
  /** maker: the wallet's own funds; covenant: released from / spent by a covenant; other: another key's */
  ownedBy: 'maker' | 'covenant' | 'other';
  /** P2SH redeem script hash of the input UTXO (null for P2PK) */
  redeemScriptHash: Hex | null;
  token: null | { ref: TokenRef; amount: bigint; owner: Hex; ownerScheme: number; leader: boolean; escrowOf: Hex | null; program: TokenProgram; witness: 'covenantId' | 'p2pk' };
  order: null | {
    kind: OrderKind | RetiredKind; template: TemplateName | RetiredKind; covenantId: Hex | null; entry: string;
    /** cancel = maker cancel, refund = expiry / kill / close refund, fill = any matching entry, update = arm / trail */
    action: 'cancel' | 'refund' | 'fill' | 'update' | 'other';
    description: OrderDescription | null; makerIsWallet: boolean;
  };
}

export interface LockedKas { total: bigint; carriers: bigint; escrow: bigint; refundTips: bigint; keeperTips: bigint; reserves: bigint }

export interface DecodedOutput {
  index: number;
  /** KAS on the output UTXO, sompi */
  value: bigint;
  kind: 'order' | 'custody' | 'token-change' | 'token-out' | 'kas-change' | 'payment-out' | 'unknown-covenant' | 'unknown';
  flagged: boolean;
  order: null | { covenantId: Hex; description: OrderDescription; verified: boolean; locked: LockedKas; deadline: bigint | null };
  token: null | { ref: TokenRef; amount: bigint; owner: Hex; ownerScheme: number; escrowOf: Hex | null };
  /** recipient key of a payment / transfer to another key */
  recipient: Hex | null;
}

/** An order that exists after the tx (placement record re-derived and verified). */
export interface DecodedOrder {
  output: number;
  covenantId: Hex;
  description: OrderDescription;
  /** KAS on the order UTXO */
  value: bigint;
  locked: LockedKas;
  custody: null | { output: number; amount: bigint; carrier: bigint; ref: TokenRef };
  /**
   * every custody the placement record names (a sell-first `KobIfdPair`: its A and its B prefund), in record order; `role` base = the order's
   * token (A), quote = a pair order's B
   */
  custodies: { output: number; amount: bigint; carrier: bigint; ref: TokenRef; role: 'base' | 'quote' }[];
  deadline: bigint | null;
  /** the output script equals the P2SH of the recovered state */
  verified: boolean;
  /** an in-place amend: the order input this output continues (the order keeps its covenant id and its custody) */
  amendedFrom?: number;
  /** a sweep in place (verified SWEEP record): the order input this output continues under the SAME script (same state, custody untouched) */
  sweptFrom?: number;
}

/** An order UTXO spent by this tx. */
export interface DecodedSpend {
  input: number;
  covenantId: Hex | null;
  description: OrderDescription | null;
  /** `sweep`: the maker's cancel continued the order in place under the same script (a verified SWEEP record): only its strays move */
  action: NonNullable<DecodedInput['order']>['action'] | 'sweep';
  makerIsWallet: boolean;
  /** custody + stray tokens of the order's own token swept by the tx (base units) */
  tokensReleased: bigint;
  /** strays of the order's own token (base units) */
  strays: bigint;
  /** strays of OTHER tokens owned by the order's covenant id and swept by this tx (a pair order's B, a retired cross limit's B; foreign strays) */
  otherStrays: { ref: TokenRef; amount: bigint; utxos?: number; foreign?: boolean }[];
  /**
   * a pair order: per token (A then B) what this spend moves of the order's own tokens: `released` (every input of that token owned by the order's
   * id), `custody` (what its state holds of that token) and `strays` (the excess, swept back); null for every other kind
   */
  pairTokens: { role: 'base' | 'quote'; ref: TokenRef; released: bigint; custody: bigint; strays: bigint }[] | null;
  /** a sweep: the output the order continues at, the stray token UTXOs it returns and the KAS they carry */
  sweptTo?: { output: number; utxos: number; kas: bigint };
  /** the order is armed / trailed by this tx: its trigger evidence (null for every other spend) */
  trigger: DecodedTrigger | null;
}

/**
 * A conditional order or stop entry armed (or trailed) by this tx, and its trigger evidence (touch, protocol v2.6): a plain resting KobAsk / KobBid
 * of the same token filled in THIS transaction. Everything is re-derived from `built.tx` / `built.plans`: the order's state and entry arguments
 * (`ev`, `tk`) from its plan, the evidence's state and fill size from the plan of input `ev`, DAA scores from the spent UTXOs, the time from the
 * lock time (the covenant proves `tx.daa >= exposedSince + minRestDaa` by CLTV against it). Prices are state prices (sompi per whole token of
 * `scale` base units, as `stopPrice`).
 */
export interface DecodedTrigger {
  /** the order input armed / trailed */
  input: number;
  covenantId: Hex | null;
  kind: OrderKind;
  /** `fill`: arms inside a fill of its own stop leg / stop entry; `update`: arms (or trails) without a fill of the order */
  path: 'fill' | 'update';
  /** what the order needs (from its state) */
  rule: {
    /** the evidence side(s) the order accepts on this path: `ask` = a resting ask filled, `bid` = a resting bid filled */
    sides: ('ask' | 'bid')[];
    stop: bigint;
    /** the smallest evidence fill, base units of a plain order of the same scale */
    minTouch: bigint;
    minRestDaa: bigint;
  };
  /** what input `ev` is (null fields when it cannot be read) */
  evidence: {
    /** the `ev` argument (-1 when missing) */
    input: number;
    /** the ask's custody input (`tk`); null for bid evidence */
    custodyInput: number | null;
    kind: OrderKind | null;
    covenantId: Hex | null;
    side: 'ask' | 'bid' | null;
    /** its quote (state price) */
    price: bigint | null;
    /** base units filled (n) */
    amount: bigint | null;
    /** max(its UTXO DAA + interval, its custody's DAA (ask), its activeFrom) */
    exposedSince: bigint | null;
  };
  /** `arm`, or `trail` (an update on opposite-side evidence) with the steps and the new stop */
  effect: 'arm' | 'trail';
  trail: { steps: bigint; newStop: bigint } | null;
  /**
   * a pair stop (`KobCondPair`, a `KobIfdPair` stop entry): its evidence mode (`kasBooks`: `evidence` is the KAS-book order of A, `evidenceB` the
   * one of B, `price` their KAS quotes; `pair`: `evidence` is a resting `KobPair` of the pair, its `price` B per whole A), the least B evidence
   * and the side of each leg the rule wants; null for the KAS kinds. Prices of the evidence are never recorded as a pair price.
   */
  pair: null | {
    mode: 'kasBooks' | 'pair' | 'invalid';
    evidenceB: DecodedTrigger['evidence'] | null;
    minTouchB: bigint | null;
    /** the sides the rule wants: mode 0 of the A and B legs, mode 1 of the pair order */
    want: { a: 'ask' | 'bid'; b: 'ask' | 'bid' | null };
  };
  /** each condition, as the covenant checks it */
  checks: { plain: boolean; sameToken: boolean; filled: boolean; custody: boolean; notDecaying: boolean; side: boolean; price: boolean; amount: boolean; rest: boolean; active: boolean };
  /** every check holds */
  ok: boolean;
}

export interface TokenNet {
  ref: TokenRef;
  /** the wallet's own token UTXOs spent */
  fromWallet: bigint;
  /** custody / stray tokens released from covenants */
  released: bigint;
  toMaker: bigint;
  escrowed: bigint;
  toOthers: bigint;
  /** toMaker - fromWallet: negative = the wallet's free balance drops */
  walletDelta: bigint;
  /** human amounts (decimals applied when the token is registered) */
  human: { fromWallet: string; released: string; toMaker: string; escrowed: string; toOthers: string; walletDelta: string };
}

export interface NetEffects {
  kas: {
    fromWallet: bigint;
    fromOthers: bigint;
    released: bigint;
    toWallet: bigint;
    locked: LockedKas;
    toOthers: bigint;
    fee: bigint;
    /** toWallet - fromWallet: negative = the wallet pays (locks + fee) */
    walletDelta: bigint;
  };
  tokens: TokenNet[];
}

/** `sweep`: the maker's sweep of an order's strays in place (the order continues unchanged, only strays move). */
export type SigningKind = 'create' | 'cancel' | 'cancel-replace' | 'cancel-position' | 'refund' | 'send' | 'sweep' | 'other';

export interface SigningSummary {
  txid: Hex;
  kind: SigningKind;
  maker: Hex;
  inputs: DecodedInput[];
  outputs: DecodedOutput[];
  /** orders that will exist */
  orders: DecodedOrder[];
  /** order UTXOs this tx spends (cancel / refund / fill ...) */
  spends: DecodedSpend[];
  /** orders this tx arms or trails, with their trigger evidence (also on their `spends` entry) */
  triggers: DecodedTrigger[];
  payload: { records: PayloadRecord[]; legacy: boolean; note: string | null } | null;
  net: NetEffects;
  /** `minimum` = the fee the builder targeted in `mode` (the node's relay floor in `relay`); `mass` as the builder measured it (display only) */
  fee: { sompi: bigint; declared: bigint; minimum: bigint; formatted: string; mode: FeeMode; mass: { fee: number; priority: number; storage: number } };
  blocking: SigningIssue[];
  warnings: SigningIssue[];
  info: SigningIssue[];
  /** no blocking finding: the UI may offer to sign (it must still show everything above) */
  ok: boolean;
  /** number of wallet signatures requested */
  signatures: number;
}

/** What the planner claims; compared against the derived facts, any disagreement is blocking. */
export interface ExpectedSigning {
  orders?: OrderState[];
  /** KAS moved into covenants (order outputs + custody token outputs), see `decodeSigning` for the accepted definitions */
  kasLocked?: bigint;
  /** base units of the orders' token moved into custodies (a pair order: of its base token A; B is `quoteEscrowed`) */
  tokensEscrowed?: bigint;
  /** covenant ids of orders spent by maker cancels / refunds */
  cancelIds?: Hex[];
  /** covenant ids of orders swept in place (verified SWEEP records): they are not cancelled */
  sweepIds?: Hex[];
  /** outpoints (`txid:index`) the tx must NOT spend: the custody of a swept order (a sweep moves strays only) */
  untouched?: string[];
  maxFee?: bigint;
  /** pair orders: B (quote token) base units moved into the new orders' custodies (a bid's escrow, a sell-first entry's prefund) */
  quoteEscrowed?: bigint;
  /** token transfers to other keys the user asked for (send): anything else leaving to another key is blocking */
  transfers?: { pubkey: Hex; amount: bigint; covenantId?: Hex }[];
  /** plain KAS payments to other keys the user asked for (sompi) */
  payments?: { pubkey: Hex; amount: bigint }[];
}

export interface DecodeInput {
  kob: KobWasm;
  built: BuiltTx;
  /** the wallet's x-only public key */
  maker: Hex;
  registry?: TokenRegistry | null;
  expected?: ExpectedSigning;
  /**
   * What the NODE says about the outpoints this tx spends (`confirmInputsOnNode`, keyed `txid:index`). Every input's amount, script and covenant
   * id must equal it: the wallet's signature commits only to the inputs it signs, so an indexer-fed amount on a covenant / token input would
   * otherwise make the fee shown here fiction. Without it (`undefined`) the tx is decoded but flagged `inputs-unverified`.
   */
  nodeInputs?: NodeInputFacts | null;
  /**
   * The wallet's fee policy. A dynamic fee at a busy moment is several times the relay floor, so with a DYNAMIC policy:
   *   * the builder's declared `fee.feeRate` must be <= max(maxRate, floor) (the floor when the policy is off): a higher one is blocking (`fee-rate-excessive`);
   *   * the absolute ceiling is raised to max(FEE_ABSOLUTE_MAX, maxFeeSompi) (a big batch at a high rate legitimately costs that);
   *   * the "a small fee is never excessive" bound (1% of the KAS moved) is raised only to what THIS transaction should cost at its own declared rate
   *     (minFee x FEE_WARN_FACTOR + FEE_WARN_SLACK), never to the whole cap: a fee far above that on a tiny move still blocks.
   * Absent = the fixed bounds and no rate check.
   */
  feePolicy?: Pick<FeePolicy, 'dynamic' | 'floor' | 'maxRate' | 'maxFeeSompi'>;
}

// ------------------------------------------------------------------------------------------------ script helpers

const P2PK_RE = /^0000 ?20([0-9a-f]{64})ac$/;
const P2SH_RE = /^0000aa20([0-9a-f]{64})87$/;
const p2pkSpk = (pk: Hex): string => `000020${pk}ac`;
const shortId = (h: string): string => `${h.slice(0, 4)}…${h.slice(-4)}`;

/** Payment key of a plain P2PK script public key (kaspa string form), null for anything else. */
export const p2pkOwner = (spk: string): Hex | null => P2PK_RE.exec(spk)?.[1] ?? null;

function tokenRef(reg: TokenRegistry | null | undefined, covenantId: Hex): TokenRef {
  const t = reg?.byCovenantId.get(covenantId);
  if (!t) return { covenantId, ticker: null, decimals: null, display: `unknown token (${shortId(covenantId)})`, inRegistry: false, tradable: false };
  return { covenantId, ticker: t.ticker, decimals: t.decimals, display: displayName(t), inRegistry: true, tradable: t.tradable };
}

const human = (ref: TokenRef, amount: bigint): string => (ref.decimals === null ? `${amount} base units` : `${formatUnits(amount, ref.decimals)} ${ref.ticker}`);

const zeroLocked = (): LockedKas => ({ total: 0n, carriers: 0n, escrow: 0n, refundTips: 0n, keeperTips: 0n, reserves: 0n });
const addLocked = (a: LockedKas, b: LockedKas): LockedKas => ({
  total: a.total + b.total, carriers: a.carriers + b.carriers, escrow: a.escrow + b.escrow, refundTips: a.refundTips + b.refundTips,
  keeperTips: a.keeperTips + b.keeperTips, reserves: a.reserves + b.reserves,
});
const min = (a: bigint, b: bigint): bigint => (a < b ? a : b);
const max0 = (a: bigint): bigint => (a < 0n ? 0n : a);

/** Splits the KAS of an order UTXO: refund tip and keeper tip (always inside the order), reserves, and carrier (asks) or trading escrow (bids). */
function splitOrderValue(d: OrderDescription, value: bigint): LockedKas {
  const refundTips = min(d.refundTip, value);
  let rest = value - refundTips;
  const keeperTips = min(d.trigger?.keeperTip ?? d.entry?.keeperTip ?? 0n, rest);
  rest -= keeperTips;
  const reserves = min(d.reservedKas, rest);
  rest -= reserves;
  // a pair order trades tokens only: its KAS is carriers (deliveries, exits) and its prefunded tip (the reserves), never a trading escrow
  return d.side === 'sell' || d.pair
    ? { total: value, carriers: rest, escrow: 0n, refundTips, keeperTips, reserves }
    : { total: value, carriers: 0n, escrow: rest, refundTips, keeperTips, reserves };
}

function planSigner(p: SigPlan): Hex | null {
  switch (p.kind) {
    case 'p2pk': return p.pubkey;
    case 'entry':
    case 'retired': return p.args.find((a) => a.kind === 'sig')?.value ?? null;
    case 'tokenLeader':
    case 'tokenDelegator': return p.witness.kind === 'p2pk' ? p.witness.value : null;
    case 'kronToken': return null; // KRON token inputs carry no signature: address presence is a P2PK input of the owner elsewhere
  }
}

function actionOfRole(role: string, entry: string): NonNullable<DecodedInput['order']>['action'] {
  const what = (role.split('@')[0].split('.')[1] ?? entry).toLowerCase();
  if (what === 'cancel') return 'cancel';
  if (what === 'refund' || what === 'close') return 'refund';
  if (what === 'update') return 'update';
  if (what === 'fill' || what === 'settle') return 'fill';
  return 'other';
}

// ------------------------------------------------------------------------------------------------ decodeSigning

export function decodeSigning(input: DecodeInput): SigningSummary {
  const { kob, built, registry = null, expected } = input;
  const maker = input.maker.toLowerCase();
  const tx = built.tx;
  const issues: SigningIssue[] = [];
  const add = (code: SigningIssueCode, message: string, extra: Partial<SigningIssue> = {}) => issues.push({ code, severity: LEVEL[code], message, ...extra });

  const empty = (): SigningSummary => ({
    txid: tx.id, kind: 'other', maker, inputs: [], outputs: [], orders: [], spends: [], triggers: [], payload: null,
    net: { kas: { fromWallet: 0n, fromOthers: 0n, released: 0n, toWallet: 0n, locked: zeroLocked(), toOthers: 0n, fee: 0n, walletDelta: 0n }, tokens: [] },
    fee: { sompi: 0n, declared: 0n, minimum: 0n, formatted: '0', mode: 'relay', mass: { fee: 0, priority: 0, storage: 0 } }, blocking: issues.filter((i) => i.severity === 'blocking'), warnings: [], info: [], ok: false, signatures: 0,
  });
  if (tx.inputs.length !== built.plans.length || tx.inputs.length !== built.roles.length || !tx.inputs.length) {
    add('malformed-built', `the transaction has ${tx.inputs.length} inputs but ${built.plans.length} signing plans and ${built.roles.length} roles`);
    return empty();
  }
  if (tx.version !== 1) add('tx-version', `transaction version ${tx.version}: KOB transactions are version 1`);

  const signBy = new Map(built.sign.map((s) => [s.inputIndex, s]));
  /** order states of the order inputs, decoded from their plans (their scripts are checked against the spent UTXOs) */
  const orderStates = new Map<number, OrderState>();

  // ---------------------------------------------------------------- inputs
  const inputs: DecodedInput[] = tx.inputs.map((inp, i) => decodeInput(i, inp, built.plans[i], built.roles[i]));
  function decodeInput(i: number, inp: TxInputJson, plan: SigPlan, role: string): DecodedInput {
    const spk = inp.utxo.scriptPublicKey;
    const hash = P2SH_RE.exec(spk)?.[1] ?? null;
    const req = signBy.get(i);
    const base = {
      index: i, role, outpoint: { txid: inp.transactionId, index: inp.index }, amount: big(inp.utxo.amount), willSign: !!req, signer: planSigner(plan),
      redeemScriptHash: hash, token: null as DecodedInput['token'], order: null as DecodedInput['order'],
    };
    const mismatch = (what: string) => add('input-script-mismatch', `input ${i}: the script of the spent UTXO is not the ${what} the plan describes`, { input: i });
    const mustMatch = (expectedSpk: string | null, what: string) => {
      if (expectedSpk === null || expectedSpk !== spk) mismatch(what);
    };
    if (plan.kind === 'p2pk') {
      mustMatch(p2pkSpk(plan.pubkey), 'P2PK script of that key');
      return { ...base, type: 'kas', ownedBy: plan.pubkey === maker ? 'maker' : 'other' };
    }
    if (plan.kind === 'entry') {
      let state: ReturnType<KobWasm['decodeState']> | null = null;
      let derivedSpk: string | null = null;
      try {
        state = kob.decodeState(plan.template, plan.state);
        derivedSpk = kob.scriptPublicKey(state);
      } catch {
        state = null;
      }
      mustMatch(derivedSpk, `${plan.template} contract of the given state`);
      const action = actionOfRole(role, plan.entry);
      if (state && isOrderKind(state.kind)) {
        const os = state as OrderState;
        orderStates.set(i, os);
        const description = describeOrder(os, kob);
        const makerIsWallet = os.state.maker === maker;
        if (action === 'cancel' && !makerIsWallet) {
          add('order-not-maker', `input ${i}: a cancel of an order that belongs to another key (${shortId(os.state.maker)}) cannot be signed by this wallet`, { input: i, params: { maker: os.state.maker } });
        }
        return {
          ...base, type: 'order', ownedBy: 'covenant',
          order: { kind: os.kind, template: plan.template, covenantId: inp.utxo.covenantId, entry: plan.entry, action, description, makerIsWallet },
        };
      }
      return { ...base, type: 'unknown', ownedBy: 'covenant', order: null };
    }
    if (plan.kind === 'retired') {
      // an order placed under an OLDER template (docs/spec/template-retirement.md): spend-only, the maker's cancel is the one entry it may take;
      // its script is derived from the retired template and the plan's state span, and its state decoded in its LEGACY lot layout (described
      // by describeLegacy: maker, token, custody; it is never re-encoded)
      let legacy: LegacyState | null = null;
      let derivedSpk: string | null = null;
      try {
        const any = kob.decodeRetired(plan.templateHash, plan.state);
        legacy = isOrderKind(any.kind) ? any : null;
        derivedSpk = kob.retiredScriptPublicKey(plan.templateHash, plan.state);
      } catch {
        legacy = null;
      }
      const state = legacy as unknown as OrderState | null;
      mustMatch(derivedSpk, `retired ${shortId(plan.templateHash)} contract of the given state`);
      if (plan.entry !== 'cancel') {
        add('retired-not-cancel', `input ${i}: an order of a retired template may only be cancelled by its maker, not '${plan.entry}'`, { input: i, params: { entry: plan.entry } });
      }
      if (!state) return { ...base, type: 'unknown', ownedBy: 'covenant', order: null };
      orderStates.set(i, state);
      const makerIsWallet = state.state.maker === maker;
      if (!makerIsWallet) {
        add('order-not-maker', `input ${i}: a cancel of an order that belongs to another key (${shortId(state.state.maker)}) cannot be signed by this wallet`, { input: i, params: { maker: state.state.maker } });
      }
      add('retired-cancel', `input ${i}: the cancel of an order placed with an older contract version (template ${shortId(plan.templateHash)})`, { input: i, params: { template: plan.templateHash } });
      return {
        ...base, type: 'order', ownedBy: 'covenant',
        order: { kind: state.kind, template: state.kind, covenantId: inp.utxo.covenantId, entry: plan.entry, action: 'cancel', description: describeLegacy(legacy!), makerIsWallet },
      };
    }
    // token leader / delegator (KCC-20) or KRON token input
    const st: TokenState = plan.state;
    let derivedSpk: string | null = null;
    try {
      derivedSpk = kob.tokenScriptPublicKey(plan.template, st);
    } catch {
      derivedSpk = null;
    }
    mustMatch(derivedSpk, `${plan.template} token script of the given state`);
    const covId = inp.utxo.covenantId ?? '';
    const ownedBy: DecodedInput['ownedBy'] = isCovenantOwned(st) ? 'covenant' : st.owner === maker ? 'maker' : 'other';
    const witness: 'covenantId' | 'p2pk' = plan.kind === 'kronToken' ? (isCovenantOwned(st) ? 'covenantId' : 'p2pk') : plan.witness.kind;
    return {
      ...base, type: 'token', ownedBy,
      token: {
        ref: tokenRef(registry, covId), amount: big(st.amount), owner: st.owner, ownerScheme: ownerTypeOf(st), leader: plan.kind === 'tokenLeader',
        escrowOf: isCovenantOwned(st) ? st.owner : null, program: plan.template, witness,
      },
    };
  }

  // signature requests must be for the wallet key, SIGHASH_ALL, on inputs whose plan wants exactly that signature
  for (const s of built.sign) {
    if (s.inputIndex < 0 || s.inputIndex >= tx.inputs.length) {
      add('sign-unexpected', `a signature is requested for input ${s.inputIndex}, which does not exist`, { input: s.inputIndex });
      continue;
    }
    if (s.pubkey !== maker) add('sign-foreign-key', `input ${s.inputIndex}: the signature request is for key ${shortId(s.pubkey)}, not the connected wallet key`, { input: s.inputIndex });
    if (s.sighashType !== 1) add('sighash-type', `input ${s.inputIndex}: sighash type ${s.sighashType} instead of SIGHASH_ALL`, { input: s.inputIndex });
    const want = planSigner(built.plans[s.inputIndex]);
    if (want === null) add('sign-unexpected', `input ${s.inputIndex}: the plan needs no signature there`, { input: s.inputIndex });
    else if (want !== s.pubkey) add('sign-mismatch', `input ${s.inputIndex}: the plan wants a signature by ${shortId(want)}, the request names ${shortId(s.pubkey)}`, { input: s.inputIndex });
  }
  for (const d of inputs) {
    if (d.signer === maker && !d.willSign) add('unsigned-maker-input', `input ${d.index} needs the wallet's signature but no signature is requested`, { input: d.index });
    if (d.type === 'kas' && d.ownedBy === 'other') add('foreign-input', `input ${d.index} spends KAS of another key (${shortId(d.signer ?? '')}) that this wallet cannot sign`, { input: d.index });
  }

  // every input's KAS value, script and covenant id must be what the node holds for that outpoint
  if (input.nodeInputs) {
    tx.inputs.forEach((inp, i) => {
      const f = input.nodeInputs!.get(outpointKey(inp.transactionId, inp.index));
      if (!f) return add('input-unconfirmed', `input ${i}: the node does not confirm this UTXO (${shortId(inp.transactionId)}:${inp.index}); its amount cannot be trusted`, { input: i });
      if (f.amount !== big(inp.utxo.amount)) add('input-unconfirmed', `input ${i}: the transaction claims ${formatKas(big(inp.utxo.amount))} KAS but the node holds ${formatKas(f.amount)} KAS on that UTXO`, { input: i, params: { claimed: big(inp.utxo.amount), node: f.amount } });
      else if (f.scriptPublicKey !== inp.utxo.scriptPublicKey || (f.covenantId ?? null) !== (inp.utxo.covenantId ?? null)) add('input-unconfirmed', `input ${i}: the script or covenant id of the spent UTXO differs from what the node holds`, { input: i });
    });
  } else {
    add('inputs-unverified', 'the KAS values of the inputs were not checked against the node: the fee shown may be wrong');
  }

  // ---------------------------------------------------------------- payload and placement records
  let recovered: RecoveredOrder[] = [];
  try {
    recovered = kob.recoverOrders(tx);
  } catch (e) {
    add('recover-failed', `a placement record in the payload does not verify: ${e instanceof Error ? e.message : String(e)}`);
  }
  let payload: SigningSummary['payload'] = null;
  if (tx.payload) {
    try {
      const p: Payload | null = kob.decodePayload(tx.payload);
      if (!p) add('payload-unknown', 'the transaction carries a payload that is not KOB1');
      else payload = { records: p.records, legacy: !!p.legacy, note: (p.records.find((r) => r.type === 'note') as { text: string } | undefined)?.text ?? null };
    } catch (e) {
      add('payload-invalid', `the payload does not decode: ${e instanceof Error ? e.message : String(e)}`);
    }
  }
  // in-place amends (AMEND records): the order's covenant id continues with a new state, proven against the state the order input spends (its plan,
  // whose script is checked against the spent UTXO above). A continuation without a verified record is an unknown covenant output (blocking).
  let amends: RecoveredAmend[] = [];
  if (payload?.records.some((r) => r.type === 'amend')) {
    try {
      amends = kob.recoverAmends(tx, built.plans);
    } catch (e) {
      add('recover-failed', `an in-place amend record in the payload does not verify: ${e instanceof Error ? e.message : String(e)}`);
    }
  }
  const amendByOutput = new Map(amends.map((a) => [a.output, a]));
  // sweeps in place (SWEEP records): the maker's cancel continues the order under the SAME script, re-checked against the order input's plan
  // (`verify_sweep`). A continuation without a verified record stays an unknown covenant output (blocking); a record that does not verify blocks.
  let sweeps: RecoveredSweep[] = [];
  if (payload?.records.some((r) => r.type === 'sweep')) {
    const r = recoverSweeps(kob, tx, built.plans);
    sweeps = r.sweeps.filter((s) => !amendByOutput.has(s.output));
    for (const f of r.failed) add('sweep-invalid', `a sweep record does not verify: ${f.reason}`, { output: f.output, input: f.input });
    if (sweeps.length < r.sweeps.length) add('sweep-invalid', 'an output is named by both an amend and a sweep record');
  }
  const sweepByOutput = new Map(sweeps.map((s) => [s.output, s]));
  const sweepByInput = new Map(sweeps.map((s) => [s.input, s]));
  // only covenants proven by a verified placement record can be custody owners: `built.covenants` is builder-declared and never trusted here
  const newCovenantIds = new Set<Hex>(recovered.map((r) => r.covenantId));
  const recoveredByOutput = new Map(recovered.map((r) => [r.output, r]));
  for (const c of built.covenants) {
    if (c.template && isOrderKind(c.template) && !c.outputs.some((o) => recoveredByOutput.has(o))) {
      add('order-unrecorded', `output ${c.outputs[0]} creates a ${c.template} covenant without a valid placement record: it would be invisible to matchers and to you`, { output: c.outputs[0] });
    }
  }

  // ---------------------------------------------------------------- outputs
  // token outputs: the leader plan of each token lists the next states in output order; outputs bound to the token's covenant id map onto them
  // (KRON has no leader: every token input carries the same next states, so the first one of each covenant id stands for the group)
  const tokenOutputs = new Map<number, { state: TokenState; program: TokenProgram; covenantId: Hex }>();
  const kronSeen = new Set<Hex>();
  built.plans.forEach((p, i) => {
    if (p.kind !== 'tokenLeader' && p.kind !== 'kronToken') return;
    const covId = tx.inputs[i].utxo.covenantId;
    if (!covId) return;
    if (p.kind === 'kronToken') {
      if (kronSeen.has(covId)) return;
      kronSeen.add(covId);
    }
    const outs = tx.outputs.map((o, j) => [o, j] as const).filter(([o]) => o.covenant?.covenantId === covId);
    if (outs.length !== p.nextStates.length) {
      add('token-output-count', `token ${shortId(covId)}: ${outs.length} outputs are bound to the token but the plan lists ${p.nextStates.length}`);
    }
    outs.forEach(([, j], k) => {
      if (p.nextStates[k]) tokenOutputs.set(j, { state: p.nextStates[k], program: p.template, covenantId: covId });
    });
  });

  const expectedTransfers = [...(expected?.transfers ?? [])];
  const expectedPayments = [...(expected?.payments ?? [])];
  // the token an order pins (covenant id, template hash, prefix / suffix, extension commitment) must be exactly the registered token
  const warnedTokens = new Set<Hex>();
  function warnToken(covenantId: Hex, output?: number) {
    if (warnedTokens.has(covenantId)) return;
    warnedTokens.add(covenantId);
    const ref = tokenRef(registry, covenantId);
    add('token-unlisted', `${ref.display} is not a tradable token in this app's registry: check its covenant id yourself`, { params: { covenantId }, ...(output !== undefined ? { output } : {}) });
  }
  function checkOrderToken(j: number, os: OrderState, rec: Pick<RecoveredOrder, 'custody' | 'prefund'>) {
    if (isPairKind(os.kind)) return checkPairTokens(j, os, rec);
    const st = os.state as unknown as Record<string, string>;
    const tk = registry?.byCovenantId.get(st.tokenCovId);
    if (!tk) return warnToken(st.tokenCovId, j);
    const bad: string[] = [];
    if (st.tokenTplHash !== tk.templateHash) bad.push('template hash');
    if (BigInt(st.tplPrefixLen) !== BigInt(tk.prefixLen) || BigInt(st.tplSuffixLen) !== BigInt(tk.suffixLen)) bad.push('template prefix / suffix length');
    if (tk.extensionCommitment !== null) {
      if (st.extensionCommitment !== undefined && st.extensionCommitment !== tk.extensionCommitment) bad.push('extension commitment');
      if (rec.custody && extensionOfState(rec.custody.state) !== tk.extensionCommitment) bad.push('custody extension commitment');
    }
    if (!tk.tradable) warnToken(tk.covenantId, j);
    if (bad.length) add('order-token-mismatch', `output ${j}: the order's token pin (${bad.join(', ')}) differs from the registered ${displayName(tk)}: it would not trade that token`, { output: j, params: { covenantId: tk.covenantId } });
  }
  /**
   * A pair order pins BOTH its tokens (covenant id, program template, prefix / suffix, family code, the extension commitment of new outputs it names)
   * and holds custodies of one or both: each pin decides what the maker sells or receives. Per token: a family code its program contradicts, or a
   * KRON token with an extension commitment, is blocking (`pair-family-mismatch`: the order could never settle); a token outside the registry is a
   * warning; a pin (or a custody's extension commitment) that differs from the token's registry entry is blocking (`pair-token-mismatch`). The same
   * token as A and B is blocking (`pair-terms-invalid`).
   */
  function checkPairTokens(j: number, os: OrderState, rec: Pick<RecoveredOrder, 'custody' | 'prefund'>) {
    const pf = pairFactsOf(os)!;
    if (pf.a.covId === pf.b.covId) add('pair-terms-invalid', `output ${j}: the pair order trades a token for itself`, { output: j });
    const custodyExt = new Map<Hex, Hex | null>();
    const cs = custodiesOf(os, kob);
    [rec.custody, rec.prefund].forEach((c, k) => {
      const tok = cs[k]?.token;
      if (c && tok) custodyExt.set(tok, extensionOfState(c.state));
    });
    for (const [role, t] of [['A', pf.a], ['B', pf.b]] as const) {
      const program = kob.templates().find((tp) => tp.hash === t.tplHash && tp.tokenSlots);
      const programFam = program ? (program.name.startsWith('Kron') ? 2n : 1n) : null;
      if (t.family === null || (programFam !== null && programFam !== t.familyCode) || (t.family === 'kron' && t.ext !== null && /[1-9a-f]/.test(t.ext))) {
        add('pair-family-mismatch', `output ${j}: token ${role} family ${t.familyCode} does not match its program${program ? ` ${program.name}` : ''} (or a KRON token carries an extension commitment)`, { output: j, params: { token: role } });
      }
      const tk = registry?.byCovenantId.get(t.covId);
      if (!tk) {
        warnToken(t.covId, j);
        continue;
      }
      const bad: string[] = [];
      if (t.tplHash !== tk.templateHash) bad.push('template hash');
      if (t.prefixLen !== BigInt(tk.prefixLen) || t.suffixLen !== BigInt(tk.suffixLen)) bad.push('template prefix / suffix length');
      if (t.family !== tk.family) bad.push('family');
      if (tk.family !== 'kron' && tk.extensionCommitment !== null) {
        if (t.ext !== null && t.ext !== tk.extensionCommitment) bad.push('extension commitment');
        const ce = custodyExt.get(t.covId);
        if (ce !== undefined && ce !== tk.extensionCommitment) bad.push('custody extension commitment');
      }
      if (!tk.tradable) warnToken(tk.covenantId, j);
      if (bad.length) {
        add('pair-token-mismatch', `output ${j}: the pair order's token ${role} pin (${bad.join(', ')}) differs from the registered ${displayName(tk)}: it would not trade that token`, {
          output: j, params: { covenantId: tk.covenantId, token: role },
        });
      }
    }
  }
  /** A NEW pair order (a placement): kob-wasm's builder rules for a new order (prices, scales, numeric gate, minimum fill) and its least KAS. */
  function checkNewPairOrder(j: number, os: OrderState, value: bigint) {
    if (!isPairKind(os.kind)) return;
    try {
      kob.checkNewOrder(os);
      const least = kob.minOrderValue(os);
      if (value < least) throw new Error(`it holds ${formatKas(value)} KAS, less than the ${formatKas(least)} KAS its carriers and tip need`);
    } catch (e) {
      add('pair-terms-invalid', `output ${j}: the pair order's terms are invalid: ${e instanceof Error ? e.message : String(e)}`, { output: j });
    }
  }

  const orders: DecodedOrder[] = [];
  const outputs: DecodedOutput[] = tx.outputs.map((out, j) => decodeOutput(j, out));

  function decodeOutput(j: number, out: TxOutputJson): DecodedOutput {
    const value = big(out.value);
    const blank = { index: j, value, flagged: false, order: null as DecodedOutput['order'], token: null as DecodedOutput['token'], recipient: null as Hex | null };
    const rec = recoveredByOutput.get(j);
    if (rec) {
      const os = rec.order as OrderState;
      let verified = false;
      try {
        verified = kob.scriptPublicKey(os) === out.scriptPublicKey && big(rec.value) === value;
      } catch {
        verified = false;
      }
      if (!verified) add('order-spk-mismatch', `output ${j}: the script is not the P2SH of the order state in the placement record`, { output: j });
      if (os.state.maker !== maker) add('order-not-maker', `output ${j}: the new order names ${shortId(os.state.maker)} as maker, not the connected wallet key: you could not cancel it`, { output: j, params: { maker: os.state.maker } });
      checkOrderToken(j, os, rec);
      checkNewPairOrder(j, os, value);
      const description = describeOrder(os, kob);
      const locked = splitOrderValue(description, value);
      // the custody parts of the record follow the state's custodies (a sell-first pair entry: A, then its B prefund), each of its own token
      const parts = custodiesOf(os, kob);
      const custodies: DecodedOrder['custodies'] = [];
      [rec.custody, rec.prefund ?? null].forEach((c, k) => {
        const out = c ? tx.outputs[c.output] : null;
        if (!c || !out) return;
        const token = parts[k]?.token ?? description.tokenCovId;
        custodies.push({ output: c.output, amount: big(c.state.amount), carrier: big(out.value), ref: tokenRef(registry, token), role: parts[k]?.role ?? 'base' });
      });
      const first = custodies[0];
      orders.push({
        output: j, covenantId: rec.covenantId, description, value, locked, deadline: rec.deadline != null ? big(rec.deadline) : null, verified,
        custody: first ? { output: first.output, amount: first.amount, carrier: first.carrier, ref: first.ref } : null, custodies,
      });
      return { ...blank, kind: 'order', flagged: !verified, order: { covenantId: rec.covenantId, description, verified, locked, deadline: rec.deadline != null ? big(rec.deadline) : null } };
    }
    const am = amendByOutput.get(j);
    if (am) {
      const os = am.order as OrderState;
      let verified = false;
      try {
        verified = kob.scriptPublicKey(os) === out.scriptPublicKey && big(am.value) === value && out.covenant?.covenantId === am.covenantId;
      } catch {
        verified = false;
      }
      if (!verified) add('order-spk-mismatch', `output ${j}: the script is not the P2SH of the amended order's state`, { output: j });
      if (os.state.maker !== maker) add('order-not-maker', `output ${j}: the amended order names ${shortId(os.state.maker)} as maker, not the connected wallet key: you could not cancel it`, { output: j, params: { maker: os.state.maker } });
      checkOrderToken(j, os, { custody: null, prefund: null });
      const description = describeOrder(os, kob);
      const locked = splitOrderValue(description, value);
      const deadline = am.deadline != null ? big(am.deadline) : null;
      orders.push({ output: j, covenantId: am.covenantId, description, value, locked, deadline, verified, custody: null, custodies: [], amendedFrom: am.input });
      return { ...blank, kind: 'order', flagged: !verified, order: { covenantId: am.covenantId, description, verified, locked, deadline } };
    }
    const sw = sweepByOutput.get(j);
    if (sw) {
      // verified above: the same script as the order input's, bound to its covenant id from that input (the same order, unchanged)
      const os = sw.order;
      if (os.state.maker !== maker) add('order-not-maker', `output ${j}: the swept order names ${shortId(os.state.maker)} as maker, not the connected wallet key`, { output: j, params: { maker: os.state.maker } });
      const description = describeOrder(os, kob);
      const locked = splitOrderValue(description, value);
      orders.push({ output: j, covenantId: sw.covenantId, description, value, locked, deadline: null, verified: true, custody: null, custodies: [], sweptFrom: sw.input });
      return { ...blank, kind: 'order', order: { covenantId: sw.covenantId, description, verified: true, locked, deadline: null } };
    }
    const tok = tokenOutputs.get(j);
    if (tok) {
      let ok = false;
      try {
        ok = kob.tokenScriptPublicKey(tok.program, tok.state) === out.scriptPublicKey;
      } catch {
        ok = false;
      }
      if (!ok) add('output-script-mismatch', `output ${j}: the script is not the ${tok.program} token script of the planned token state`, { output: j });
      const st = tok.state;
      const ref = tokenRef(registry, tok.covenantId);
      const token = { ref, amount: big(st.amount), owner: st.owner, ownerScheme: ownerTypeOf(st), escrowOf: isCovenantOwned(st) ? st.owner : null };
      const unplain = unplainTokenState(st);
      if (unplain) add('token-state-unplain', `output ${j}: the token state carries ${unplain}: tokens with these features could be moved by someone else`, { output: j });
      if (isCovenantOwned(st)) {
        if (newCovenantIds.has(st.owner)) return { ...blank, kind: 'custody', flagged: !ok, token };
        add('token-to-unknown-owner', `output ${j}: ${human(ref, token.amount)} would be locked in covenant ${shortId(st.owner)}, which this transaction does not create: the tokens could be lost`, { output: j, params: { owner: st.owner } });
        return { ...blank, kind: 'unknown-covenant', flagged: true, token };
      }
      if (isKeyOwned(st)) {
        if (st.owner === maker) return { ...blank, kind: 'token-change', flagged: !ok, token };
        const ix = expectedTransfers.findIndex((t) => t.pubkey === st.owner && t.amount === token.amount && (!t.covenantId || t.covenantId === tok.covenantId));
        if (ix >= 0) {
          expectedTransfers.splice(ix, 1);
          add('transfer-expected', `output ${j}: ${human(ref, token.amount)} go to ${shortId(st.owner)} as requested`, { output: j });
          return { ...blank, kind: 'token-out', flagged: false, token, recipient: st.owner };
        }
        add('transfer-out', `output ${j}: ${human(ref, token.amount)} would be TRANSFERRED to another key (${shortId(st.owner)})`, { output: j, params: { recipient: st.owner, amount: token.amount } });
        return { ...blank, kind: 'token-out', flagged: true, token, recipient: st.owner };
      }
      add('token-to-unknown-owner', `output ${j}: ${human(ref, token.amount)} would go to an owner of unknown type (${ownerTypeOf(st)})`, { output: j });
      return { ...blank, kind: 'unknown-covenant', flagged: true, token };
    }
    const pk = p2pkOwner(out.scriptPublicKey);
    if (pk && !out.covenant) {
      if (pk === maker) return { ...blank, kind: 'kas-change' };
      const ix = expectedPayments.findIndex((p) => p.pubkey === pk && p.amount === value);
      if (ix >= 0) {
        expectedPayments.splice(ix, 1);
        add('payment-expected', `output ${j}: ${formatKas(value)} KAS go to ${shortId(pk)} as requested`, { output: j });
        return { ...blank, kind: 'payment-out', recipient: pk };
      }
      add('payment-out', `output ${j}: ${formatKas(value)} KAS would be PAID to another key (${shortId(pk)})`, { output: j, params: { recipient: pk, amount: value } });
      return { ...blank, kind: 'payment-out', flagged: true, recipient: pk };
    }
    add('output-unknown', `output ${j}: ${formatKas(value)} KAS would go to a script this app cannot identify${out.covenant ? ' (a covenant output)' : ''}`, { output: j });
    return { ...blank, kind: out.covenant ? 'unknown-covenant' : 'unknown', flagged: true };
  }

  // ---------------------------------------------------------------- triggers (touch evidence of armed / trailed orders)
  /**
   * The trigger of order input `i` spent through `plan`, or null when this spend arms nothing. Mirrors the covenants (contracts/v2 KobCondAsk /
   * KobCondBid / KobIfdBid / KobIfdAsk `settle` / `fill` / `update` and their `touch*` functions), whose entry signatures are
   *   KobCondAsk.settle(nb, tokenIn, tokOut, leg, ev, tk, t)          KobCondBid.settle(nb, tokenTplIn, leg, ev, t)
   *   KobIfdBid.fill(nb, tokenTplIn, exitOut, cPre, cSuf, ev, t)       KobIfdAsk.settle(nb, tokenIn, tokOut, exitOut, cPre, cSuf, ev, tk, t)
   *   update(ev, tk) (KobIfdBid: update(ev)); tk >= 0 = ask evidence with its custody at tk, otherwise bid evidence.
   */
  function triggerOf(i: number, os: OrderState, plan: Extract<SigPlan, { kind: 'entry' }>, covenantId: Hex | null): DecodedTrigger | null {
    const s = os.state as unknown as Record<string, string>;
    const base = baseKind(os.kind);
    const args = plan.args;
    const int = (k: number): bigint | null => {
      const a = args[k];
      return a && a.kind === 'int' && /^-?\d+$/.test(a.value) ? BigInt(a.value) : null;
    };
    const armed = big(s.armed ?? '0');
    let path: 'fill' | 'update';
    let ev: bigint | null = null;
    let tk: bigint | null = null;
    let stop = 0n;
    if (plan.entry === 'update') {
      if (base !== 'KobCondAsk' && base !== 'KobCondBid' && base !== 'KobIfdBid' && base !== 'KobIfdAsk') return null;
      path = 'update';
      ev = int(0);
      tk = base === 'KobIfdBid' ? null : int(1);
      stop = big(base === 'KobIfdBid' || base === 'KobIfdAsk' ? s.entryStop : s.stopPrice);
    } else {
      const n = fillArg(args[0]);
      if (n === null || n <= 0n || armed !== 0n) return null;
      path = 'fill';
      if (base === 'KobCondAsk' && plan.entry === 'settle' && int(3) === 1n && big(s.stopPrice) > 0n) [ev, tk, stop] = [int(4), int(5), big(s.stopPrice)];
      else if (base === 'KobCondBid' && plan.entry === 'settle' && int(2) === 1n && big(s.stopPrice) > 0n) [ev, stop] = [int(3), big(s.stopPrice)];
      else if (base === 'KobIfdBid' && plan.entry === 'fill' && big(s.entryStop) > 0n) [ev, stop] = [int(5), big(s.entryStop)];
      else if (base === 'KobIfdAsk' && plan.entry === 'settle' && big(s.entryStop) > 0n) [ev, tk, stop] = [int(6), int(7), big(s.entryStop)];
      else return null;
    }
    // a sell stop / sell-stop entry arms on a resting ASK at or below its stop, a buy stop / buy-stop entry on a resting BID at or above it; an
    // update of a stop ORDER also trails on the other side (KobCondAsk up on resting bids, KobCondBid down on resting asks)
    const armSide: 'ask' | 'bid' = base === 'KobCondAsk' || base === 'KobIfdAsk' ? 'ask' : 'bid';
    const sides: ('ask' | 'bid')[] = path === 'update' && (base === 'KobCondAsk' || base === 'KobCondBid') ? ['ask', 'bid'] : [armSide];
    const minTouch = big(s.minTouch);
    const minRestDaa = big(s.minRestDaa);
    const lock = big(tx.lockTime);
    const nIn = BigInt(tx.inputs.length);
    const evIdx = ev !== null && ev >= 0n && ev < nIn && Number(ev) !== i ? Number(ev) : -1;
    // the arguments name the side the covenant authenticates at ev: an ask when tk >= 0 (its custody at tk), a bid otherwise
    const argSide: 'ask' | 'bid' = tk !== null && tk >= 0n ? 'ask' : 'bid';
    const fam = familyOfKind(os.kind);
    const eState = evIdx >= 0 ? orderStates.get(evIdx) : undefined;
    const ePlan = evIdx >= 0 ? built.plans[evIdx] : undefined;
    // plain: a KobAsk / KobBid of the order's family (KobCross, conditional and if-done fills are never evidence)
    const plain = !!eState && (eState.kind === kindFor('KobAsk', fam) || eState.kind === kindFor('KobBid', fam));
    const es = plain ? (eState!.state as unknown as Record<string, string>) : null;
    const side: 'ask' | 'bid' | null = es ? (baseKind(eState!.kind) === 'KobAsk' ? 'ask' : 'bid') : null;
    // the same token AND the same scale (prices comparable)
    const sameToken = !!es && es.tokenCovId === s.tokenCovId && big(es.scale) === big(s.scale);
    // the evidence is FILLED here: its own fill entry (KobAsk.settle / KobBid.fill) with n > 0 (its script enforces the fill for that n)
    const amount = es && ePlan?.kind === 'entry' && ePlan.entry === (side === 'ask' ? 'settle' : 'fill') ? fillArg(ePlan.args[0]) : null;
    const filled = amount !== null && amount > 0n;
    const evInput = evIdx >= 0 ? tx.inputs[evIdx]! : null;
    const evCov = evInput?.utxo.covenantId ?? null;
    // an ask's custody (tk): the token input of this token owned by the ask's covenant id
    const custodyInput: number | null = argSide === 'ask' ? Number(tk) : null;
    let custody = true;
    if (side === 'ask' && argSide === 'ask') {
      const c = custodyInput !== null && custodyInput < tx.inputs.length ? inputs[custodyInput]!.token : null;
      custody = !!c && !!evCov && c.escrowOf === evCov && c.ref.covenantId === s.tokenCovId;
    }
    const price = es ? big(es.price) : null;
    const notDecaying = !!es && big(es.slope) === 0n;
    let exposedSince: bigint | null = null;
    if (es && evInput) {
      let x = big(evInput.utxo.blockDaaScore) + big(es.interval);
      if (big(es.activeFrom) > x) x = big(es.activeFrom);
      if (side === 'ask' && custodyInput !== null && custodyInput < tx.inputs.length) {
        const td = big(tx.inputs[custodyInput]!.utxo.blockDaaScore);
        if (td > x) x = td;
      }
      exposedSince = x;
    }
    let effect: 'arm' | 'trail' = 'arm';
    let trail: DecodedTrigger['trail'] = null;
    let priceOk = false;
    // the side must be one the order accepts on this path AND the one its arguments name (the template the covenant reads)
    const sideOk = side !== null && side === argSide && sides.includes(side);
    if (sideOk && price !== null) {
      if (side === armSide) {
        priceOk = armSide === 'ask' ? price <= stop : price >= stop;
      } else {
        effect = 'trail';
        const steps = trailSteps(base === 'KobCondAsk', s, price);
        trail = { steps, newStop: base === 'KobCondAsk' ? stop + steps * big(s.trailStep) : stop - steps * big(s.trailStep) };
        priceOk = steps >= 1n;
      }
    }
    const checks = {
      plain, sameToken, filled, custody, notDecaying, side: sideOk, price: priceOk, amount: amount !== null && amount >= minTouch,
      rest: exposedSince !== null && exposedSince + minRestDaa <= lock, active: lock >= big(s.activeFrom),
    };
    return {
      input: i, covenantId, kind: os.kind, path, rule: { sides, stop, minTouch, minRestDaa },
      evidence: { input: ev === null ? -1 : Number(ev), custodyInput, kind: eState?.kind ?? null, covenantId: evCov, side, price, amount, exposedSince },
      effect, trail, checks, ok: Object.values(checks).every(Boolean), pair: null,
    };
  }
  /**
   * The trigger of a pair stop (`KobCondPair` stop leg, `KobIfdPair` stop entry) spent through `plan`, or null when this spend arms nothing.
   * Entries (contracts/v2 KobCondPair / KobIfdPair headers):
   *   KobCondPair.settle(nb, custIn, tTplIn, t, sOut, tOut, leg, evA, evB, tk, evMode, upd, k)
   *   KobIfdPair.fill(nb, aIn, bIn, aTplIn, bTplIn, exitOut, amt, cPre, cSuf, evA, evB, tk, evMode, t, aOut, bOut, xc, upd)
   * nb > 0 with upd 0 on an unarmed stop (a conditional's leg 1) arms in the fill; nb 0 with upd 1 is the update (a conditional's k = 0 arms, k >= 1
   * trails by k steps). Evidence mode 0 (evMode 0): plain resting KAS-book orders of A (input evA) and of B (input evB) filled here, the ask leg's
   * custody at tk; mode 1: a resting `KobPair` of this pair (A, B, both scales) filled here (input evA), its custody at tk. The rule (sides, least
   * evidence, rest) is kob-wasm `pairTriggerRule`; the price test kob-wasm `pairArms` / `condPairTrailCheck`.
   */
  function pairTriggerOf(i: number, os: OrderState, plan: Extract<SigPlan, { kind: 'entry' }>, covenantId: Hex | null): DecodedTrigger | null {
    const cond = os.kind === 'KobCondPair';
    if (!cond && os.kind !== 'KobIfdPair') return null;
    if (plan.entry !== (cond ? 'settle' : 'fill')) return null;
    const s = os.state as unknown as Record<string, string>;
    const args = plan.args;
    const int = (k: number): bigint | null => {
      const a = args[k];
      return a && a.kind === 'int' && /^-?\d+$/.test(a.value) ? BigInt(a.value) : null;
    };
    const [iEvA, iEvB, iTk, iMode, iUpd] = cond ? [7, 8, 9, 10, 11] : [9, 10, 11, 12, 17];
    const n = fillArg(args[0]);
    const upd = int(iUpd);
    const armed = big(s.armed ?? '0');
    const stop = big(cond ? s.stopPrice : s.entryStop);
    let path: 'fill' | 'update';
    let effect: 'arm' | 'trail' = 'arm';
    let k = 0n;
    if (upd === 1n && n === 0n) {
      path = 'update';
      if (cond) {
        k = int(12) ?? -1n;
        if (k >= 1n) effect = 'trail';
      }
    } else if (n !== null && n > 0n && upd === 0n && armed === 0n && stop > 0n && (!cond || int(6) === 1n)) path = 'fill';
    else return null;
    const pf = pairFactsOf(os)!;
    let rule: PairTriggerRule | null = null;
    try {
      rule = kob.pairTriggerRule(os);
    } catch {
      rule = null;
    }
    const modeArg = int(iMode);
    const mode: 'kasBooks' | 'pair' | 'invalid' = modeArg === 0n ? 'kasBooks' : modeArg === 1n ? 'pair' : 'invalid';
    const sides = effect === 'trail' ? (rule?.trail ?? null) : (rule?.arm ?? null);
    const wantA: 'ask' | 'bid' = sides ? (mode === 'pair' ? sides.pair : sides.kasBooks.a) : 'ask';
    const wantB: 'ask' | 'bid' | null = sides && mode === 'kasBooks' ? sides.kasBooks.b : null;
    const lock = big(tx.lockTime);
    const nIn = BigInt(tx.inputs.length);
    const at = (v: bigint | null): number => (v !== null && v >= 0n && v < nIn && Number(v) !== i ? Number(v) : -1);
    const tkIdx = at(int(iTk));
    const minTouch = big(s.minTouch);
    const minRestDaa = big(s.minRestDaa);
    const minTouchB = rule?.minTouchB != null ? big(rule.minTouchB) : null;

    /** one evidence read: a plain KAS-book order of `token` (mode 0) or a resting KobPair of this pair (mode 1) at input ev */
    const read = (ev: number, token: PairTokenFacts | null): { e: DecodedTrigger['evidence']; plain: boolean; same: boolean; filled: boolean; custody: boolean; flat: boolean } => {
      const eState = ev >= 0 ? orderStates.get(ev) : undefined;
      const ePlan = ev >= 0 ? built.plans[ev] : undefined;
      const evInput = ev >= 0 ? tx.inputs[ev]! : null;
      const evCov = evInput?.utxo.covenantId ?? null;
      const es = eState ? (eState.state as unknown as Record<string, string>) : null;
      let plain = false;
      let same = false;
      let side: 'ask' | 'bid' | null = null;
      let custodyToken: Hex | null = null;
      if (eState && token) {
        // mode 0: the KobAsk / KobBid of the token's own family
        const fam = token.family ?? 'kcc20';
        plain = eState.kind === kindFor('KobAsk', fam) || eState.kind === kindFor('KobBid', fam);
        if (plain) {
          side = baseKind(eState.kind) === 'KobAsk' ? 'ask' : 'bid';
          same = es!.tokenCovId === token.covId && big(es!.scale) === token.scale;
          custodyToken = side === 'ask' ? token.covId : null;
        }
      } else if (eState && !token) {
        // mode 1: a KobPair of exactly this pair (A, B, scale(A), scale(B))
        plain = eState.kind === 'KobPair';
        const ef = plain ? pairFactsOf(eState) : null;
        if (ef) {
          side = ef.side === 'sell' ? 'ask' : 'bid';
          same = ef.a.covId === pf.a.covId && ef.b.covId === pf.b.covId && ef.a.scale === pf.a.scale && ef.b.scale === pf.b.scale;
          custodyToken = ef.side === 'sell' ? ef.a.covId : ef.b.covId;
        }
      }
      // FILLED here: its own fill entry (KobAsk.settle, KobBid.fill, KobPair.settle) with n > 0 (its script enforces the fill for that n)
      const fillEntry = side === 'bid' && token ? 'fill' : 'settle';
      const amount = plain && ePlan?.kind === 'entry' && ePlan.entry === fillEntry ? fillArg(ePlan.args[0]) : null;
      // its custody (an ask leg, a pair order): input tk, a token input of that token owned by the evidence's covenant id
      let custody = true;
      let custodyInput: number | null = null;
      if (custodyToken !== null) {
        custodyInput = tkIdx >= 0 ? tkIdx : null;
        const c = custodyInput !== null ? inputs[custodyInput]!.token : null;
        custody = !!c && !!evCov && c.escrowOf === evCov && c.ref.covenantId === custodyToken;
      }
      let exposedSince: bigint | null = null;
      if (es && evInput && plain) {
        let x = big(evInput.utxo.blockDaaScore) + big(es.interval ?? '0');
        if (big(es.activeFrom ?? '0') > x) x = big(es.activeFrom);
        if (custodyInput !== null) {
          const td = big(tx.inputs[custodyInput]!.utxo.blockDaaScore);
          if (td > x) x = td;
        }
        exposedSince = x;
      }
      return {
        e: { input: ev, custodyInput, kind: eState?.kind ?? null, covenantId: evCov, side, price: es && plain ? big(es.price) : null, amount, exposedSince },
        plain, same, filled: amount !== null && amount > 0n, custody, flat: !!es && plain && big(es.slope ?? '0') === 0n,
      };
    };
    const evA = at(int(iEvA));
    const evB = mode === 'kasBooks' ? at(int(iEvB)) : -1;
    const A = read(evA, mode === 'pair' ? null : pf.a);
    const B = mode === 'kasBooks' ? read(evB, pf.b) : null;
    // exactly one leg of mode 0 is an ask (its custody is tk)
    const custodyOk = mode === 'kasBooks' ? (A.e.side === 'ask' ? A.custody : B!.custody) && (A.e.side !== B!.e.side) : A.custody;
    const sideOk = sides !== null && A.e.side === wantA && (B === null || B.e.side === wantB);
    let priceOk = false;
    let trail: DecodedTrigger['trail'] = null;
    if (sideOk && A.e.price !== null && (B === null || B.e.price !== null)) {
      const ev: PairEvidence = mode === 'pair' ? { mode: 'pair', price: A.e.price.toString() } : { mode: 'kasBooks', a: A.e.price.toString(), b: B!.e.price!.toString() };
      try {
        if (effect === 'trail') {
          priceOk = kob.condPairTrailCheck(os, ev, k);
          const step = big(s.trailStep);
          trail = { steps: k, newStop: pf.side === 'sell' ? stop + k * step : stop - k * step };
        } else priceOk = kob.pairArms(os, ev) === true;
      } catch {
        priceOk = false;
      }
    }
    const amountOk = A.e.amount !== null && A.e.amount >= minTouch && (B === null || (B.e.amount !== null && minTouchB !== null && B.e.amount >= minTouchB));
    const rested = (e: DecodedTrigger['evidence']) => e.exposedSince !== null && e.exposedSince + minRestDaa <= lock;
    const checks = {
      plain: mode !== 'invalid' && A.plain && (B === null || B.plain), sameToken: A.same && (B === null || B.same), filled: A.filled && (B === null || B.filled),
      custody: custodyOk, notDecaying: A.flat && (B === null || B.flat), side: sideOk, price: priceOk, amount: amountOk,
      rest: rested(A.e) && (B === null || rested(B.e)), active: lock >= big(s.activeFrom),
    };
    return {
      input: i, covenantId, kind: os.kind, path, rule: { sides: [wantA], stop, minTouch, minRestDaa },
      evidence: A.e, effect, trail, checks, ok: Object.values(checks).every(Boolean),
      pair: { mode, evidenceB: B?.e ?? null, minTouchB, want: { a: wantA, b: wantB } },
    };
  }
  /** Findings of a pair trigger: invalid evidence or an unmet rule is blocking; a valid one is shown (info). */
  function reportPairTrigger(t: DecodedTrigger, where: string, input: number) {
    const x = t.pair!;
    const params = { evidence: t.evidence.input, mode: x.mode };
    const legs = x.evidenceB ? [t.evidence, x.evidenceB] : [t.evidence];
    const legName = (e: DecodedTrigger['evidence']) => `input ${e.input}`;
    if (x.mode === 'invalid' || !t.checks.plain || !t.checks.sameToken || !t.checks.filled || !t.checks.custody) {
      const why = x.mode === 'invalid'
        ? 'its evidence mode is neither 0 (two KAS-book fills) nor 1 (a resting pair order)'
        : !t.checks.plain
          ? x.mode === 'pair'
            ? `${legName(t.evidence)} is not a resting KobPair${t.evidence.kind ? ` (it is a ${t.evidence.kind})` : ''}`
            : `${legs.map(legName).join(' / ')} must be plain KobAsk / KobBid orders of the KAS books of A and of B`
          : !t.checks.sameToken
            ? x.mode === 'pair' ? `${legName(t.evidence)} is a pair order of another pair or scale` : 'an evidence order trades another token or unit'
            : !t.checks.filled
              ? 'an evidence order is not filled by this transaction'
              : 'the custody input named for the evidence is not that order\'s custody';
      add('trigger-evidence-invalid', `${where}: its trigger evidence is invalid: ${why}. Only two KAS-book fills (one of A, one of B) or the fill of a resting pair order of the same pair arms a pair stop`, { input, params });
      return;
    }
    const bad: string[] = [];
    if (!t.checks.side) {
      bad.push(x.mode === 'pair'
        ? `the pair order is a resting ${t.evidence.side}, but the rule needs a resting ${x.want.a}`
        : `the KAS-book legs are a resting ${t.evidence.side} of A and ${x.evidenceB?.side} of B, but the rule needs a resting ${x.want.a} of A and ${x.want.b} of B`);
    } else if (!t.checks.price) {
      bad.push(t.effect === 'trail'
        ? `the evidence does not justify exactly ${t.trail?.steps ?? 0n} trailing step(s)`
        : x.mode === 'pair'
          ? `its price ${t.evidence.price} is not ${x.want.a === 'ask' ? 'at or below' : 'at or above'} the stop ${t.rule.stop}`
          : `the implied rate of the quotes ${t.evidence.price} (A) and ${x.evidenceB?.price} (B) is not ${x.want.a === 'ask' ? 'at or below' : 'at or above'} the stop ${t.rule.stop}`);
    }
    if (!t.checks.amount) bad.push(`the evidence fills ${t.evidence.amount} base units of A${x.evidenceB ? ` and ${x.evidenceB.amount} of B` : ''}, below minTouch ${t.rule.minTouch}${x.minTouchB !== null && x.evidenceB ? ` / ${x.minTouchB}` : ''}`);
    if (!t.checks.notDecaying) bad.push('an evidence order decays (not a quote)');
    if (!t.checks.rest) bad.push(`an evidence order rested less than minRestDaa ${t.rule.minRestDaa} before the lock time`);
    if (!t.checks.active) bad.push('the order is not active yet');
    if (bad.length) {
      add('trigger-rule-unmet', `${where}: the evidence does not satisfy the order's trigger rule: ${bad.join('; ')}`, { input, params });
      return;
    }
    const what = x.mode === 'pair'
      ? `the fill of a resting pair ${t.evidence.side} (input ${t.evidence.input}) at ${t.evidence.price} B per whole A, ${t.evidence.amount} base units of A`
      : `the fills of a resting ${t.evidence.side} of A (input ${t.evidence.input}, ${t.evidence.price} sompi) and a resting ${x.evidenceB?.side} of B (input ${x.evidenceB?.input}, ${x.evidenceB?.price} sompi)`;
    add('trigger-evidence', `${where} is ${t.effect === 'trail' ? `trailed to stop ${t.trail!.newStop}` : 'armed'} by ${what} (needs ${t.rule.minTouch} of A)`, { input, params });
  }
  const triggers = new Map<number, DecodedTrigger>();
  for (const d of inputs) {
    const plan = built.plans[d.index]!;
    const os = orderStates.get(d.index);
    if (!os || !d.order || plan.kind !== 'entry') continue;
    const t = isPairKind(os.kind) ? pairTriggerOf(d.index, os, plan, d.order.covenantId) : triggerOf(d.index, os, plan, d.order.covenantId);
    if (!t) continue;
    triggers.set(d.index, t);
    const where = `input ${d.index} (${t.kind}.${t.path === 'update' ? 'update' : plan.entry})`;
    const params = { evidence: t.evidence.input };
    if (t.pair) {
      reportPairTrigger(t, where, d.index);
      continue;
    }
    if (!t.checks.plain || !t.checks.sameToken || !t.checks.filled || !t.checks.custody) {
      const fam = familyOfKind(t.kind);
      const why = !t.checks.plain
        ? `input ${t.evidence.input} is not a plain ${kindFor('KobAsk', fam)} / ${kindFor('KobBid', fam)} order${t.evidence.kind ? ` (it is a ${t.evidence.kind})` : ''}`
        : !t.checks.sameToken
          ? `input ${t.evidence.input} trades another token or unit`
          : !t.checks.filled
            ? `input ${t.evidence.input} is not filled by this transaction`
            : `input ${t.evidence.custodyInput} is not the custody of the evidence ask`;
      add('trigger-evidence-invalid', `${where}: its trigger evidence is invalid: ${why}. Only the fill of a plain resting order of the same token arms a stop`, { input: d.index, params });
      continue;
    }
    const bad: string[] = [];
    if (!t.checks.side) bad.push(`it is a resting ${t.evidence.side}, but the order needs a resting ${t.rule.sides.join(' or ')} and its arguments name a resting ${t.evidence.custodyInput !== null ? 'ask' : 'bid'}`);
    else if (!t.checks.price) bad.push(t.effect === 'trail' ? `its quote ${t.evidence.price} justifies no trailing step` : `its quote ${t.evidence.price} is not ${t.rule.sides[0] === 'ask' ? 'at or below' : 'at or above'} the stop ${t.rule.stop}`);
    if (!t.checks.amount) bad.push(`${t.evidence.amount} base units filled, below minTouch ${t.rule.minTouch}`);
    if (!t.checks.notDecaying) bad.push('it decays (not a quote)');
    if (!t.checks.rest) bad.push(`it rested since DAA ${t.evidence.exposedSince}, less than minRestDaa ${t.rule.minRestDaa} before the lock time`);
    if (!t.checks.active) bad.push('the order is not active yet');
    if (bad.length) {
      add('trigger-rule-unmet', `${where}: the fill of input ${t.evidence.input} does not satisfy the order's trigger rule: ${bad.join('; ')}`, { input: d.index, params });
      continue;
    }
    add('trigger-evidence', `${where} is ${t.effect === 'trail' ? `trailed to stop ${t.trail!.newStop}` : 'armed'} by the fill of input ${t.evidence.input}: a resting ${t.evidence.side} quoting ${t.evidence.price}, ${t.evidence.amount} base units (needs ${t.rule.minTouch})`, {
      input: d.index, params,
    });
  }

  // ---------------------------------------------------------------- spends (order inputs)
  const spends: DecodedSpend[] = inputs
    .filter((d) => d.order)
    .map((d) => {
      const o = d.order!;
      const cov = o.covenantId;
      const owned = inputs.filter((t) => t.token && t.token.escrowOf && t.token.escrowOf === cov);
      const sum = (xs: DecodedInput[]): bigint => xs.reduce((a, t) => a + t.token!.amount, 0n);
      const sw = sweepByInput.get(d.index);
      const swept = !!sw && o.action === 'cancel';
      // the order's own token(s): its token (a pair order: A and B, each with its custody per the state) and, apart, FOREIGN strays of any other
      // token owned by the same id (matcher.md 1.2)
      const pair = o.description?.pair ?? null;
      const ownToken = o.description?.tokenCovId ?? null;
      const quoteToken = pair && pair.quote.covId !== pair.base.covId ? pair.quote.covId : null;
      const mine = owned.filter((t) => ownToken === null || t.token!.ref.covenantId === ownToken);
      const total = sum(mine);
      const heldOf = (token: Hex): bigint => (pair ? pair.custodies.filter((c) => c.token === token).reduce((x, c) => x + c.amount, 0n) : (o.description?.tokenAmount ?? 0n));
      const expectedCustody = ownToken !== null ? heldOf(ownToken) : 0n;
      const strayOf = (released: bigint, held: bigint): bigint => (swept ? released : released > held ? released - held : 0n);
      let pairTokens: DecodedSpend['pairTokens'] = null;
      if (pair) {
        pairTokens = [{ role: 'base' as const, token: pair.base.covId }, ...(quoteToken ? [{ role: 'quote' as const, token: quoteToken }] : [])].map(({ role, token }) => {
          const released = sum(owned.filter((t) => t.token!.ref.covenantId === token));
          const custody = heldOf(token);
          return { role, ref: tokenRef(registry, token), released, custody, strays: strayOf(released, custody) };
        });
      }
      const others = new Map<Hex, { ref: TokenRef; amount: bigint; utxos: number; foreign: boolean }>();
      for (const t of owned) {
        if (mine.includes(t)) continue;
        const k = t.token!.ref.covenantId;
        // B of a pair order (or of a retired cross limit) is one of its own tokens: only its EXCESS over the custody the state holds is a stray
        if (k === quoteToken) continue;
        const cur = others.get(k) ?? { ref: t.token!.ref, amount: 0n, utxos: 0, foreign: true };
        cur.amount += t.token!.amount;
        cur.utxos++;
        others.set(k, cur);
      }
      const quote = pairTokens?.find((x) => x.role === 'quote');
      if (quote && quote.strays > 0n) {
        const n = owned.filter((t) => t.token!.ref.covenantId === quoteToken).length;
        others.set(quote.ref.covenantId, { ref: quote.ref, amount: quote.strays, utxos: swept ? n : Math.max(n - (quote.custody > 0n ? 1 : 0), 0), foreign: false });
      }
      // a verified sweep: the maker's cancel continued the order under the same script; every token input owned by it is a stray (its custody,
      // owned by the same id, must stay where it is: `expected.untouched`)
      if (swept) {
        return {
          input: d.index, covenantId: cov, description: o.description, action: 'sweep' as const, makerIsWallet: o.makerIsWallet,
          tokensReleased: total, strays: total, otherStrays: [...others.values()], pairTokens, trigger: null,
          sweptTo: { output: sw!.output, utxos: owned.length, kas: owned.reduce((x, t) => x + t.amount, 0n) },
        };
      }
      return {
        input: d.index, covenantId: cov, description: o.description, action: o.action, makerIsWallet: o.makerIsWallet,
        tokensReleased: total, strays: strayOf(total, expectedCustody), otherStrays: [...others.values()], pairTokens,
        trigger: triggers.get(d.index) ?? null,
      };
    });
  for (const s of spends) {
    if (s.strays > 0n) add('strays-swept', `input ${s.input}: ${s.strays} base units of stray tokens sent to this order are swept back to you`, { input: s.input, params: { amount: s.strays } });
    for (const x of s.otherStrays) {
      add('other-strays-swept', `input ${s.input}: ${human(x.ref, x.amount)} of ${x.ref.display} sent to this order are swept back to you`, { input: s.input, params: { amount: human(x.ref, x.amount), token: x.ref.display } });
    }
    if (s.sweptTo) {
      add('swept-in-place', `input ${s.input}: the order continues unchanged at output ${s.sweptTo.output} (same script: price, amount and custody); only its ${s.sweptTo.utxos} stray token UTXO(s) move back to you`, {
        input: s.input, output: s.sweptTo.output, params: { utxos: s.sweptTo.utxos },
      });
    }
  }
  // a sweep moves strays only: an outpoint the plan names as the swept order's custody must not be spent
  for (const u of expected?.untouched ?? []) {
    const i = tx.inputs.findIndex((x) => `${x.transactionId}:${x.index}` === u);
    if (i >= 0) add('sweep-custody-spent', `input ${i}: the transaction spends ${shortId(u.split(':')[0] ?? '')}:${u.split(':')[1] ?? ''}, the swept order's custody, which a sweep must leave in place`, { input: i });
  }

  // ---------------------------------------------------------------- net effects
  const sum = (xs: bigint[]): bigint => xs.reduce((a, b) => a + b, 0n);
  const fromWallet = sum(inputs.filter((d) => d.ownedBy === 'maker').map((d) => d.amount));
  const fromOthers = sum(inputs.filter((d) => d.ownedBy === 'other').map((d) => d.amount));
  const released = sum(inputs.filter((d) => d.ownedBy === 'covenant').map((d) => d.amount));
  const toWallet = sum(outputs.filter((o) => o.kind === 'kas-change' || o.kind === 'token-change').map((o) => o.value));
  const toOthers = sum(outputs.filter((o) => ['payment-out', 'token-out', 'unknown-covenant', 'unknown'].includes(o.kind)).map((o) => o.value));
  let locked = zeroLocked();
  for (const o of orders) locked = addLocked(locked, o.locked);
  for (const o of outputs.filter((x) => x.kind === 'custody')) locked = addLocked(locked, { ...zeroLocked(), total: o.value, carriers: o.value });
  const totalIn = sum(inputs.map((d) => d.amount));
  const totalOut = sum(outputs.map((o) => o.value));
  const derivedFee = totalIn - totalOut;
  if (derivedFee < 0n) add('kas-unbalanced', `the outputs (${formatKas(totalOut)} KAS) exceed the inputs (${formatKas(totalIn)} KAS)`);
  const declaredFee = big(built.fee.fee);
  const minimumFee = big(built.fee.minFee);
  if (derivedFee !== declaredFee) add('fee-mismatch', `the inputs minus the outputs leave a fee of ${formatKas(derivedFee)} KAS but the builder reports ${formatKas(declaredFee)} KAS`, { params: { derived: derivedFee, declared: declaredFee } });
  const moved = locked.total + released + toOthers;
  const policy = input.feePolicy;
  const dynamic = !!policy && policy.dynamic;
  if (policy) {
    let declaredRate: bigint | null;
    try {
      declaredRate = BigInt(built.fee.feeRate);
    } catch {
      declaredRate = null;
    }
    const limit = declaredRateLimit(policy);
    if (declaredRate === null || declaredRate > limit) {
      add('fee-rate-excessive', `the builder declares a fee rate of ${built.fee.feeRate} sompi per gram, above the ${limit} this wallet allows`, { params: { rate: built.fee.feeRate, max: limit } });
    }
  }
  const absoluteMax = dynamic && policy!.maxFeeSompi > FEE_ABSOLUTE_MAX ? policy!.maxFeeSompi : FEE_ABSOLUTE_MAX;
  const feeCeiling = (built.fee.feeMode ?? 'relay') === 'priority' && minimumFee * 3n > absoluteMax ? minimumFee * 3n : absoluteMax;
  const atOwnRate = minimumFee * FEE_WARN_FACTOR + FEE_WARN_SLACK;
  const feeFloorOk = dynamic && atOwnRate > FEE_FLOOR_OK ? atOwnRate : FEE_FLOOR_OK;
  if (derivedFee > feeCeiling) {
    add('fee-excessive', `the network fee of ${formatKas(derivedFee)} KAS is above the absolute ceiling of ${formatKas(feeCeiling)} KAS`, { params: { fee: derivedFee, moved } });
  } else if (derivedFee > feeFloorOk && derivedFee * 100n > moved * FEE_MAX_PERCENT) {
    add('fee-excessive', `the network fee of ${formatKas(derivedFee)} KAS is more than ${FEE_MAX_PERCENT}% of the ${formatKas(moved)} KAS this transaction moves`, { params: { fee: derivedFee, moved } });
  } else if (derivedFee > minimumFee * FEE_WARN_FACTOR + FEE_WARN_SLACK) {
    add('fee-high', `the network fee (${formatKas(derivedFee)} KAS) is much higher than the minimum for this transaction (${formatKas(minimumFee)} KAS)`, { params: { fee: derivedFee, minimum: minimumFee } });
  }

  // tokens: conservation per token, then per-token effects
  const tokenIds = new Set<Hex>();
  for (const d of inputs) if (d.token) tokenIds.add(d.token.ref.covenantId);
  for (const o of outputs) if (o.token) tokenIds.add(o.token.ref.covenantId);
  const tokens: TokenNet[] = [];
  for (const id of [...tokenIds].sort()) {
    const ref = tokenRef(registry, id);
    const ins = inputs.filter((d) => d.token?.ref.covenantId === id);
    const outs = outputs.filter((o) => o.token?.ref.covenantId === id);
    const inSum = sum(ins.map((d) => d.token!.amount));
    const outSum = sum(outs.map((o) => o.token!.amount));
    if (inSum !== outSum) add('token-unbalanced', `${ref.display}: ${inSum} base units go in but ${outSum} come out: tokens would be created or destroyed`, { params: { in: inSum, out: outSum } });
    if (!ref.tradable && ins.length + outs.length > 0) warnToken(id);
    const fw = sum(ins.filter((d) => d.ownedBy === 'maker').map((d) => d.token!.amount));
    const rel = sum(ins.filter((d) => d.ownedBy === 'covenant').map((d) => d.token!.amount));
    const toMaker = sum(outs.filter((o) => o.kind === 'token-change').map((o) => o.token!.amount));
    const esc = sum(outs.filter((o) => o.kind === 'custody').map((o) => o.token!.amount));
    const oth = sum(outs.filter((o) => o.kind === 'token-out' || o.kind === 'unknown-covenant').map((o) => o.token!.amount));
    const delta = toMaker - fw;
    tokens.push({
      ref, fromWallet: fw, released: rel, toMaker, escrowed: esc, toOthers: oth, walletDelta: delta,
      human: { fromWallet: human(ref, fw), released: human(ref, rel), toMaker: human(ref, toMaker), escrowed: human(ref, esc), toOthers: human(ref, oth), walletDelta: (delta < 0n ? '-' : '') + human(ref, delta < 0n ? -delta : delta) },
    });
  }

  // ---------------------------------------------------------------- expected (planner claims) vs derived
  if (expected) {
    if (expected.maxFee !== undefined && derivedFee > expected.maxFee) {
      add('expected-max-fee', `the fee ${formatKas(derivedFee)} KAS exceeds the ${formatKas(expected.maxFee)} KAS the plan allowed`, { params: { fee: derivedFee, max: expected.maxFee } });
    }
    if (expected.orders) compareOrders(expected.orders);
    if (expected.kasLocked !== undefined) {
      const tokenChangeCarriers = sum(outputs.filter((o) => o.kind === 'token-change').map((o) => o.value));
      const netOut = fromWallet - toWallet - derivedFee;
      const accepted = [locked.total, locked.total + tokenChangeCarriers, netOut];
      if (!accepted.includes(expected.kasLocked)) {
        add('expected-kas-locked', `the plan says ${formatKas(expected.kasLocked)} KAS are locked but the transaction locks ${formatKas(locked.total)} KAS`, { params: { expected: expected.kasLocked, actual: locked.total } });
      }
    }
    // with a pair order the custodies count by role: A (the orders' token) against `tokensEscrowed`, B against `quoteEscrowed`
    const byRole = orders.some((o) => o.description.pair);
    const roleSum = (role: 'base' | 'quote') => sum(orders.flatMap((o) => o.custodies.filter((c) => c.role === role).map((c) => c.amount)));
    if (expected.tokensEscrowed !== undefined) {
      const escrowed = byRole ? roleSum('base') : sum(tokens.map((t) => t.escrowed));
      if (escrowed !== expected.tokensEscrowed) add('expected-tokens-escrowed', `the plan says ${expected.tokensEscrowed} token base units are escrowed but the transaction escrows ${escrowed}`, { params: { expected: expected.tokensEscrowed, actual: escrowed } });
    }
    if (expected.quoteEscrowed !== undefined) {
      const escrowed = roleSum('quote');
      if (escrowed !== expected.quoteEscrowed) add('expected-quote-escrowed', `the plan says ${expected.quoteEscrowed} base units of the quote token are escrowed but the transaction escrows ${escrowed}`, { params: { expected: expected.quoteEscrowed, actual: escrowed } });
    }
    if (expected.cancelIds) {
      const actual = spends.filter((s) => s.action === 'cancel' || s.action === 'refund').map((s) => s.covenantId ?? '');
      const a = [...actual].sort().join(',');
      const e = [...expected.cancelIds].sort().join(',');
      if (a !== e) add('expected-cancel-ids', `the plan cancels ${expected.cancelIds.length} order(s) but the transaction spends ${actual.length}: ${actual.map(shortId).join(', ') || 'none'}`, { params: { expected: expected.cancelIds.length, actual: actual.length } });
    }
    // a sweep plan names the orders it sweeps in place: the transaction must sweep exactly those (and a cancel plan none)
    const swept = spends.filter((s) => s.action === 'sweep').map((s) => s.covenantId ?? '');
    if (expected.sweepIds || (expected.cancelIds && swept.length)) {
      const want = expected.sweepIds ?? [];
      if ([...swept].sort().join(',') !== [...want].sort().join(',')) {
        add('expected-sweep-ids', `the plan sweeps ${want.length} order(s) in place but the transaction sweeps ${swept.length}: ${swept.map(shortId).join(', ') || 'none'}`, { params: { expected: want.length, actual: swept.length } });
      }
    }
  }
  function compareOrders(exp: OrderState[]) {
    const enc = (s: OrderState): string => {
      try {
        return kob.encodeState(s);
      } catch {
        return '';
      }
    };
    const have = orders.map((o) => enc((recoveredByOutput.get(o.output)?.order ?? amendByOutput.get(o.output)?.order ?? sweepByOutput.get(o.output)!.order) as OrderState));
    const pool = [...have];
    const leftovers: OrderState[] = [];
    for (const e of exp) {
      const i = pool.indexOf(enc(e));
      if (i >= 0) pool.splice(i, 1);
      else leftovers.push(e);
    }
    // an if-done entry's committed exit is part of the plan's states but is created later by fills: it must match the entry's commitment
    const exits = orders.map((o) => o.description.entry?.exit).filter((x): x is OrderDescription => !!x);
    const rest = leftovers.filter((e) => {
      const ix = exits.findIndex((x) => sameExit(x, describeOrder(e)));
      if (ix < 0) return true;
      exits.splice(ix, 1);
      return false;
    });
    if (rest.length || pool.length) {
      add('expected-orders', `the plan describes ${exp.length} order(s) but the transaction's placement records describe ${orders.length}, and they differ (${rest.length} planned order(s) missing, ${pool.length} unplanned)`, { params: { planned: exp.length, actual: orders.length } });
    }
  }
  function sameExit(a: OrderDescription, b: OrderDescription): boolean {
    // what a fill writes into the exit (amount, custody, repeat fields; a pair exit's tip prefund follows its amount) is not part of the commitment
    const strip = (d: OrderDescription) => ({
      ...d, amountLeft: null, tokenAmount: null, booked: null, entry: null,
      ...(d.pair ? { reservedKas: null, pair: { ...d.pair, custody: null, custodies: null, escrowA: null, escrowB: null, triggerRule: null } } : {}),
    });
    return JSON.stringify(strip(a), bigReplacer) === JSON.stringify(strip(b), bigReplacer);
  }

  // ---------------------------------------------------------------- classification
  // a swept order continues unchanged: this tx does not create it
  const createdOrders = orders.some((o) => o.sweptFrom === undefined);
  const cancels = spends.filter((s) => s.action === 'cancel');
  const refunds = spends.filter((s) => s.action === 'refund');
  const sweptSpends = spends.filter((s) => s.action === 'sweep');
  const tokenOnly = spends.length === 0 && !createdOrders && tokens.length > 0;
  const kind: SigningKind = createdOrders && cancels.length ? 'cancel-replace' : createdOrders ? 'create' : cancels.length > 1 ? 'cancel-position' : cancels.length === 1 ? 'cancel' : refunds.length ? 'refund' : sweptSpends.length ? 'sweep' : tokenOnly ? 'send' : 'other';
  if (createdOrders && !payload) add('no-order-records', 'orders are created but the payload carries no KOB1 records');
  if (cancels.length && released > 0n) add('kas-released', `${formatKas(released)} KAS held by the cancelled order(s) return to you`, { params: { released } });

  const severity = (s: IssueLevel) => issues.filter((i) => i.severity === s);
  return {
    txid: tx.id, kind, maker, inputs, outputs, orders, spends, triggers: [...triggers.values()], payload,
    net: {
      kas: { fromWallet, fromOthers, released, toWallet, locked, toOthers, fee: derivedFee, walletDelta: toWallet - fromWallet },
      tokens,
    },
    fee: {
      sompi: derivedFee, declared: declaredFee, minimum: minimumFee, formatted: formatKas(derivedFee), mode: built.fee.feeMode ?? 'relay',
      mass: { fee: built.fee.mass.feeMass, priority: built.fee.mass.priorityMass ?? built.fee.mass.feeMass, storage: built.fee.mass.storage },
    },
    blocking: severity('blocking'), warnings: severity('warning'), info: severity('info'), ok: severity('blocking').length === 0, signatures: built.sign.length,
  };
}

/** What makes a token output more than plain ownership (a borrow guard or a minter flag), or null. */
function unplainTokenState(st: TokenState): string | null {
  if ('is_minter' in st) return st.is_minter ? 'the minter flag' : null;
  if (st.borrow_scheme !== 0) return 'a borrow scheme';
  if (/[1-9a-f]/.test(st.borrow_guard)) return 'a borrow guard';
  return null;
}

const bigReplacer = (_k: string, v: unknown) => (typeof v === 'bigint' ? v.toString() : v);

/**
 * The fill argument `nb` of a KobAsk / KobBid / conditional / if-done entry: an 8-byte little-endian push. Positive = base units filled; the top bit set
 * (a repeat merge's `-(i * 2^32 + n)`) or 0 (refund) is no fill. Null when the argument is not 8 bytes.
 */
export function fillArg(a: SigArg | undefined): bigint | null {
  if (!a || a.kind !== 'bytes' || !/^[0-9a-f]{16}$/i.test(a.value)) return null;
  let v = 0n;
  for (let k = 7; k >= 0; k--) v = (v << 8n) | BigInt(parseInt(a.value.slice(2 * k, 2 * k + 2), 16));
  return v >= 1n << 63n ? -(v & ((1n << 63n) - 1n)) : v;
}

/**
 * Trailing steps an update justifies (kob-protocol `CondAskState::trail_steps` / `CondBidState::trail_steps`): a sell stop ratchets UP on a resting
 * bid quoting `rp` (k = floor((rp - gap - stop) / step), capped below the take-profit), a buy stop DOWN on a resting ask (k = floor((stop - gap - rp)
 * / step), capped above the take-profit and 0). 0 = not justified.
 */
export function trailSteps(sellStop: boolean, s: Record<string, string>, rp: bigint): bigint {
  const step = big(s.trailStep);
  if (step <= 0n) return 0n;
  const stop = big(s.stopPrice);
  const gap = big(s.trailGap);
  const tp = big(s.tpPrice);
  const fdiv = (a: bigint, b: bigint): bigint => (a >= 0n ? a / b : -((-a + b - 1n) / b));
  let k: bigint;
  if (sellStop) {
    k = fdiv(rp - gap - stop, step);
    if (tp > 0n) {
      const cap = fdiv(tp - 1n - stop, step);
      if (cap < k) k = cap;
    }
  } else {
    k = fdiv(stop - gap - rp, step);
    const kf = fdiv(stop - (tp > 0n ? tp : 0n) - 1n, step);
    if (kf < k) k = kf;
  }
  return k > 0n ? k : 0n;
}

// ------------------------------------------------------------------------------------------------ what the wallet popup will (not) show

export interface WalletNotice { code: string; message: string; wallet?: WalletId; params?: Record<string, string | number> }
export interface WalletInputHint { index: number; kind: 'p2pk' | 'covenant'; willSign: boolean; needsRedeemScript: boolean }
export interface WalletDisclosure {
  inputs: WalletInputHint[];
  /** wallet signatures requested */
  signCount: number;
  /** inputs the wallet is NOT asked to sign: covenant / token inputs authorised by their scripts, not by a key */
  unsignedCovenantInputs: number;
  /** things the popup does not (or wrongly) show, in user-facing English; the UI translates by `code` */
  notices: WalletNotice[];
}

/**
 * The blind-signing notice: which inputs the wallet is asked to sign and what its popup will NOT show. Behaviour observed on TN10
 * (tools/wallet-gate/RESULTS.md). `wallet` narrows the notices to one wallet; without it the generic ones are returned.
 */
export function describeInputsForWallet(built: BuiltTx, wallet?: WalletId): WalletDisclosure {
  const signBy = new Map(built.sign.map((s) => [s.inputIndex, s]));
  const inputs: WalletInputHint[] = built.plans.map((p, i) => ({
    index: i, kind: p.kind === 'p2pk' ? 'p2pk' : 'covenant', willSign: signBy.has(i), needsRedeemScript: !!signBy.get(i)?.redeemScript,
  }));
  const covenant = inputs.filter((i) => i.kind === 'covenant');
  const unsigned = covenant.filter((i) => !i.willSign).length;
  const signedCovenant = covenant.filter((i) => i.willSign).length;
  const notices: WalletNotice[] = [];
  const n = (code: string, message: string, w?: WalletId, params?: Record<string, string | number>) => {
    if (!wallet || !w || w === wallet) notices.push({ code, message, ...(w ? { wallet: w } : {}), ...(params ? { params } : {}) });
  };
  if (covenant.length) {
    n('blind-tokens', 'Your wallet does not understand KOB orders or tokens. It will not show token amounts, order prices or where tokens go. Check this screen, not the popup.');
    n('kasware-spend', 'KasWare shows "Sign Transaction" with KAS amounts and a generic "Spend" output; it shows no token amount or owner change.', 'kasware');
    n('kaspire-covenant', 'Kaspire warns that an input is a covenant or non-standard script it cannot verify; that warning is expected here. Token semantics appear nowhere in it.', 'kaspire');
    n('kastle-balance', 'Kastle may show covenant inputs as "Change to your balance" and covenant outputs as "Sending amount": ignore those labels.', 'kastle');
    if (signedCovenant) n('kastle-scripts', 'Kastle needs the redeem script of each covenant input to sign it; the app supplies it.', 'kastle', { inputs: signedCovenant });
  }
  if (unsigned) {
    n('covenant-inputs-unsigned', `${unsigned} input(s) are covenant or token inputs released by their scripts; the wallet is not asked to sign them and they still move.`, undefined, { inputs: unsigned });
  }
  if (!built.sign.length) n('no-signature', 'This transaction needs no wallet signature: it is authorised by the contracts themselves.');
  return { inputs, signCount: built.sign.length, unsignedCovenantInputs: unsigned, notices };
}
