// The private-key signer's one-approval merge session: the pure rules (no DOM, no key). The frame decodes a whole merge plan (kob/merge.ts
// `planMerge`, every link built against provisional outpoints), the user allows it once, and each later `sign` that carries the session id is
// signed without a prompt only while this frame's own decoding finds it to be exactly the next link of that plan:
//
// - a pure self-merge of one token (`mergeFacts`): every input signed by this key (so every input amount is committed, and the fee is
//   inputs minus outputs), one token output to this key holding the whole amount, at most one KAS change to this key, nothing else, no
//   blocking finding and no warning beyond the two a frame without a node or registry always gets;
// - the next link of the plan the user allowed (`admitStep`): the same original UTXOs (outpoint, KAS and token amount) and the outputs of
//   the links this session signed before, the same merged amount and carrier, a fee within the per-transaction cap and the budget left,
//   no input spent twice, before the session expires.
//
// Anything else ends the session and is shown for confirmation as usual. A KRON token input carries no signature, so a KRON merge never
// qualifies (founder 2026-11-14: KRON stays one approval per transaction).
import type { DecodedInput, SigningSummary } from '../kob/decode';
import { MAX_TOKEN_INPUTS } from '../kob/guards';
import { tokenCarrierFloor } from '../kob/orders/common';
import { borrowKeyOf, extensionOfState, keyState, withBorrowOf } from '../kob/token-state';
import type { BuiltTx, Hex, TokenState } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { mergedTokens } from './summary';

/** The most transactions one session covers (kob/merge.ts MAX_MERGE_TXS: a test keeps them equal). */
export const SESSION_MAX_TXS = 50;
/** Slack per transaction on top of twice the planned fee: a later link is rebuilt at the rate then. */
export const SESSION_FEE_SLACK = 100_000n;
/** The per-transaction fee cap never exceeds 0.2 KAS. */
export const SESSION_TX_FEE_CAP = 20_000_000n;
/** The fee budget of a whole session never exceeds 5 KAS. */
export const SESSION_FEE_CAP = 500_000_000n;
/** A session lasts 2 min plus 10 s per transaction, at most 10 min. */
export const SESSION_BASE_MS = 120_000;
export const SESSION_PER_TX_MS = 10_000;
export const SESSION_MAX_MS = 600_000;
/** The most KAS inputs of one merge link (the builder adds a few for the fee at most). */
const MAX_KAS_INPUTS = 9;
const ONE_KAS = 100_000_000n;
/** The warnings a frame without a node or a registry always gets: every other warning refuses. */
const EXPECTED_WARNINGS = new Set<string>(['inputs-unverified', 'token-unlisted']);
const ZERO_SUBNETWORK = /^0{40}$/;

/** Why a transaction or a plan is not admitted (tests and the frame's own reasoning; never shown as text). */
export type Refusal =
  | 'blocking' | 'warning' | 'info' | 'shape' | 'not-merge' | 'input' | 'unsigned' | 'output' | 'net' | 'fee' | 'carrier' | 'token'
  | 'plan-size' | 'plan-token' | 'plan-reuse' | 'plan-parent' | 'plan-fee'
  | 'expired' | 'count' | 'deviates' | 'consumed' | 'fee-cap' | 'fee-budget';

/** A token input as the session binds it: its outpoint, the KAS on it and its token amount. */
export interface BoundUtxo {
  outpoint: string;
  kas: bigint;
  amount: bigint;
}

/** What one admitted merge transaction does, from this frame's decoding. */
export interface MergeFacts {
  covenantId: Hex;
  program: string;
  tokenInputs: BoundUtxo[];
  /** outpoints of the key's own KAS inputs */
  kasInputs: string[];
  /** the one token output: its index, KAS and token amount */
  output: { index: number; kas: bigint; amount: bigint };
  /** the borrow setting (kob/token-state.ts `borrowKeyOf`) every token input carries and the merged output keeps */
  setting: string;
  fee: bigint;
}

const outpoint = (i: { transactionId: string; index: number }): string => `${i.transactionId}:${i.index}`;
const refuse = (why: Refusal): { refused: Refusal } => ({ refused: why });

/**
 * The facts of `built` when it is a pure self-merge of one token of `pubkey` that this frame may sign under a session (see the head of this
 * file), else why not. `decoded` is this frame's decoding of `built` for `pubkey` (summary.ts `summarize`). Fails closed: anything not
 * known to be a merge refuses.
 */
export function mergeFacts(kob: Pick<KobWasm, 'templates' | 'tokenScriptPublicKey'>, decoded: SigningSummary, built: BuiltTx, pubkey: Hex): MergeFacts | { refused: Refusal } {
  if (decoded.blocking.length > 0) return refuse('blocking');
  if (decoded.warnings.some((w) => !EXPECTED_WARNINGS.has(w.code))) return refuse('warning');
  if (decoded.info.length > 0) return refuse('info');
  const tx = built.tx;
  if (tx.version !== 1 || tx.payload !== '' || String(tx.lockTime) !== '0' || String(tx.gas) !== '0' || !ZERO_SUBNETWORK.test(tx.subnetworkId)) return refuse('shape');
  if (built.covenants.length > 0 || decoded.orders.length > 0 || decoded.spends.length > 0 || decoded.triggers.length > 0) return refuse('not-merge');
  const merged = mergedTokens(decoded);
  if (!merged || merged.length !== 1) return refuse('not-merge');
  const m = merged[0]!;
  if (m.outCount !== 1 || m.inCount < 2 || m.inCount > MAX_TOKEN_INPUTS) return refuse('not-merge');
  const covenantId = m.ref.covenantId;

  // every input is this key's and signed by it: then every input amount is committed, and so is the fee
  if (decoded.signatures !== tx.inputs.length || built.sign.length !== tx.inputs.length || built.sign.some((s) => s.pubkey !== pubkey)) return refuse('unsigned');
  if (decoded.inputs.length !== tx.inputs.length) return refuse('input');
  const tokenInputs: BoundUtxo[] = [];
  const kasInputs: string[] = [];
  let program: string | null = null;
  let largest = 0n;
  // the token state of the first token input (its plan's), which every other token input must share in borrow setting and extension
  let first: TokenState | null = null;
  for (const d of decoded.inputs as DecodedInput[]) {
    const raw = tx.inputs[d.index];
    if (!raw || !d.willSign || d.signer !== pubkey || d.ownedBy !== 'maker') return refuse('unsigned');
    if (d.type === 'token') {
      const k = d.token;
      if (!k || k.ref.covenantId !== covenantId || k.owner !== pubkey || k.ownerScheme !== 0 || k.escrowOf !== null || k.witness !== 'p2pk' || raw.utxo.covenantId !== covenantId) return refuse('input');
      if (program !== null && k.program !== program) return refuse('input');
      program = k.program;
      const plan = built.plans[d.index];
      if (!plan || (plan.kind !== 'tokenLeader' && plan.kind !== 'tokenDelegator')) return refuse('input');
      if (first && (borrowKeyOf(plan.state) !== borrowKeyOf(first) || extensionOfState(plan.state) !== extensionOfState(first))) return refuse('input');
      first ??= plan.state;
      tokenInputs.push({ outpoint: outpoint(raw), kas: d.amount, amount: k.amount });
      if (d.amount > largest) largest = d.amount;
    } else if (d.type === 'kas') {
      if (raw.utxo.covenantId || d.redeemScriptHash !== null) return refuse('input');
      kasInputs.push(outpoint(raw));
    } else return refuse('input');
  }
  if (program === null || first === null || tokenInputs.length !== m.inCount || kasInputs.length > MAX_KAS_INPUTS) return refuse('input');

  // outputs: the one merged token UTXO and at most one KAS change, both this key's, nothing flagged
  let output: MergeFacts['output'] | null = null;
  let change = 0;
  for (const o of decoded.outputs) {
    if (o.flagged) return refuse('output');
    if (o.kind === 'token-change' && o.token && o.token.ref.covenantId === covenantId && o.token.owner === pubkey && o.token.ownerScheme === 0 && o.token.escrowOf === null && !output) {
      output = { index: o.index, kas: o.value, amount: o.token.amount };
    } else if (o.kind === 'kas-change' && o.token === null && change === 0) change++;
    else return refuse('output');
  }
  if (!output || output.amount !== m.amount || decoded.outputs.length !== tx.outputs.length) return refuse('output');
  // the merged output is exactly the state kob/merge.ts plans: the whole amount, this key, the inputs' extension and borrow setting (a
  // setting dropped or changed on the way is not a merge)
  const want = withBorrowOf(keyState('kcc20', m.amount, pubkey, extensionOfState(first), program as never), first);
  if (kob.tokenScriptPublicKey(program as never, want) !== tx.outputs[output.index]?.scriptPublicKey) return refuse('output');

  const k = decoded.net.kas;
  if (k.toOthers !== 0n || k.locked.total !== 0n || k.released !== 0n || k.fromOthers !== 0n) return refuse('net');
  if (decoded.net.tokens.some((x) => x.toOthers !== 0n || x.escrowed !== 0n || x.released !== 0n || x.walletDelta !== 0n)) return refuse('net');

  // the fee: inputs minus outputs, as the builder reports it
  const sumIn = tx.inputs.reduce((s, i) => s + BigInt(i.utxo.amount), 0n);
  const sumOut = tx.outputs.reduce((s, o) => s + BigInt(o.value), 0n);
  const fee = sumIn - sumOut;
  if (fee <= 0n || fee !== k.fee || fee !== BigInt(built.fee.fee)) return refuse('fee');

  // the merged UTXO keeps the largest input carrier, at most 1 KAS, at least the program's floor (kob/merge.ts `mergedCarrier`): the
  // wallet's KAS is never parked on a token UTXO
  const tpl = kob.templates().find((x) => x.name === program && x.tokenSlots);
  if (!tpl) return refuse('token');
  const floor = tokenCarrierFloor(kob as KobWasm, tpl.hash);
  const capped = largest < ONE_KAS ? largest : ONE_KAS;
  if (output.kas > (capped > floor ? capped : floor)) return refuse('carrier');

  return { covenantId, program, tokenInputs, kasInputs, output, fee, setting: borrowKeyOf(first) };
}

/** One link of the allowed plan, as each step is checked against it. */
export interface PlannedLink {
  /** the wallet's original UTXOs it merges, by outpoint */
  fresh: Map<string, BoundUtxo>;
  /** the earlier links whose merged outputs it spends */
  parents: number[];
  /** the merged token amount and the KAS its output keeps */
  amount: bigint;
  carrier: bigint;
  fee: bigint;
  /** the borrow setting it merges in and keeps */
  setting: string;
}

/** What the user allows: derived by this frame from the plan it decoded, never taken from the app. */
export interface SessionTerms {
  covenantId: Hex;
  program: string;
  links: PlannedLink[];
  /** the planned fees, and the caps: per transaction and for the whole session */
  plannedFee: bigint;
  perTxFeeMax: bigint;
  feeMax: bigint;
  ttlMs: number;
  /** the original UTXOs the plan merges, their token amount, and the UTXOs it leaves */
  utxos: number;
  amount: bigint;
  outputs: number;
}

/**
 * The terms of a session over `plan` (the links in run order, each with its facts), or why there is none: every link a merge of the same
 * token, no outpoint spent twice, a link's other token inputs the merged outputs of earlier links of the plan (`plan[j].tx.id`, the
 * provisional id the plan is built against), within the hard caps.
 */
export function sessionTerms(plan: readonly { built: BuiltTx; facts: MergeFacts }[]): SessionTerms | { refused: Refusal } {
  const n = plan.length;
  if (n < 1 || n > SESSION_MAX_TXS) return refuse('plan-size');
  const first = plan[0]!.facts;
  if (plan.some((l) => l.facts.covenantId !== first.covenantId || l.facts.program !== first.program)) return refuse('plan-token');
  const spent = new Set<string>();
  const planIds = new Set(plan.map((l) => l.built.tx.id));
  if (planIds.size !== n) return refuse('plan-reuse');
  const produced = new Map<string, { link: number; kas: bigint; amount: bigint }>();
  plan.forEach((l, j) => produced.set(`${l.built.tx.id}:${l.facts.output.index}`, { link: j, kas: l.facts.output.kas, amount: l.facts.output.amount }));
  const links: PlannedLink[] = [];
  let utxos = 0;
  let amount = 0n;
  for (let k = 0; k < n; k++) {
    const f = plan[k]!.facts;
    for (const o of [...f.tokenInputs.map((u) => u.outpoint), ...f.kasInputs]) {
      if (spent.has(o)) return refuse('plan-reuse');
      spent.add(o);
    }
    const fresh = new Map<string, BoundUtxo>();
    const parents: number[] = [];
    for (const u of f.tokenInputs) {
      const p = produced.get(u.outpoint);
      if (!p) {
        // an original UTXO of the wallet, never another output of a transaction of the plan
        if (planIds.has(u.outpoint.split(':')[0]!)) return refuse('plan-parent');
        fresh.set(u.outpoint, u);
        continue;
      }
      if (p.link >= k || p.kas !== u.kas || p.amount !== u.amount) return refuse('plan-parent');
      parents.push(p.link);
    }
    // a KAS input of the plan is the wallet's own coin, never an output of the plan itself
    if (f.kasInputs.some((o) => planIds.has(o.split(':')[0]!))) return refuse('plan-parent');
    utxos += fresh.size;
    for (const u of fresh.values()) amount += u.amount;
    links.push({ fresh, parents, amount: f.output.amount, carrier: f.output.kas, fee: f.fee, setting: f.setting });
  }
  const leaves = links.filter((_, j) => !links.some((l) => l.parents.includes(j))).length;
  const plannedFee = links.reduce((s, l) => s + l.fee, 0n);
  const largestFee = links.reduce((m, l) => (l.fee > m ? l.fee : m), 0n);
  const min = (a: bigint, b: bigint): bigint => (a < b ? a : b);
  const perTxFeeMax = min(2n * largestFee + SESSION_FEE_SLACK, SESSION_TX_FEE_CAP);
  const feeMax = min(min(2n * plannedFee + BigInt(n) * SESSION_FEE_SLACK, BigInt(n) * perTxFeeMax), SESSION_FEE_CAP);
  if (largestFee > perTxFeeMax || plannedFee > feeMax) return refuse('plan-fee');
  const ttlMs = Math.min(SESSION_BASE_MS + SESSION_PER_TX_MS * n, SESSION_MAX_MS);
  return { covenantId: first.covenantId, program: first.program, links, plannedFee, perTxFeeMax, feeMax, ttlMs, utxos, amount, outputs: leaves };
}

/** A live session: the terms and what the run has used of them. Held in the frame's memory only. */
export interface SessionState {
  sid: string;
  pubkey: Hex;
  terms: SessionTerms;
  /** when it expires on the frame's monotonic clock, and on the wall clock (a monotonic clock can stand still while the device sleeps) */
  expiresAt: number;
  expiresAtWall: number;
  /** every outpoint a step of this session spent */
  consumed: Set<string>;
  /** the merged output of each signed link, with the transaction id this frame's own finalize gave it */
  signed: BoundUtxo[];
  feeLeft: bigint;
}

/** Opens the state of a session the user allowed at `now` (the frame's monotonic clock) and `wall` (its wall clock). */
export function openState(sid: string, pubkey: Hex, terms: SessionTerms, now: number, wall: number): SessionState {
  return { sid, pubkey, terms, expiresAt: now + terms.ttlMs, expiresAtWall: wall + terms.ttlMs, consumed: new Set(), signed: [], feeLeft: terms.feeMax };
}

/**
 * Whether `facts` (the decoded step) is exactly the next link of the allowed plan at `now` / `wall` (it expires on whichever clock passes
 * first): null when it is, else why not. The step's token
 * inputs must be the link's original UTXOs as allowed and the merged outputs of the links this session signed, nothing else, its merged amount
 * and carrier the planned ones, its fee within both caps, none of its inputs spent by an earlier step.
 */
export function admitStep(s: SessionState, facts: MergeFacts, now: number, wall: number): Refusal | null {
  if (now >= s.expiresAt || wall >= s.expiresAtWall) return 'expired';
  const k = s.signed.length;
  const link = s.terms.links[k];
  if (!link) return 'count';
  if (facts.covenantId !== s.terms.covenantId || facts.program !== s.terms.program) return 'deviates';
  if ([...facts.tokenInputs.map((u) => u.outpoint), ...facts.kasInputs].some((o) => s.consumed.has(o))) return 'consumed';
  if (facts.tokenInputs.length !== link.fresh.size + link.parents.length) return 'deviates';
  const parents = link.parents.map((p) => s.signed[p]);
  if (parents.some((p) => !p)) return 'deviates';
  const used = new Set<string>();
  for (const u of facts.tokenInputs) {
    const want = link.fresh.get(u.outpoint) ?? parents.find((p) => p!.outpoint === u.outpoint);
    if (!want || used.has(u.outpoint) || want.kas !== u.kas || want.amount !== u.amount) return 'deviates';
    used.add(u.outpoint);
  }
  if (facts.output.amount !== link.amount || facts.output.kas !== link.carrier || facts.setting !== link.setting) return 'deviates';
  if (facts.fee > s.terms.perTxFeeMax) return 'fee-cap';
  if (facts.fee > s.feeLeft) return 'fee-budget';
  return null;
}

/** Records a step once it is signed (core.ts; a failure to sign ends the session instead): its inputs spent, its fee debited, its output kept. */
export function recordStep(s: SessionState, facts: MergeFacts, txid: Hex): void {
  for (const o of [...facts.tokenInputs.map((u) => u.outpoint), ...facts.kasInputs]) s.consumed.add(o);
  s.feeLeft -= facts.fee;
  s.signed.push({ outpoint: `${txid}:${facts.output.index}`, kas: facts.output.kas, amount: facts.output.amount });
}

/** How many transactions the session still covers. */
export const stepsLeft = (s: SessionState): number => s.terms.links.length - s.signed.length;
