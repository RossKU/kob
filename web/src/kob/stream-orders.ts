// Reads what a block's transaction does to the order book from the chain alone, for standalone
// mode (data/standalone.ts). The module has no protocol logic of its own.
//
// - Placements are re-derived with kob-wasm `recoverOrders`, one record at a time as the indexer
//   does (crates/kobd/src/indexer/processor.rs). Each ORDER record of a payload is checked alone,
//   so one bad record does not hide the others.
// - A spend of an order is read from its input's signature script, as the indexer's
//   `parse_reveal` does. The redeem script is the last push. It counts as an order of a pinned
//   template only when kob-wasm re-derives exactly that script from the state it holds
//   (`decodeState` then `redeemScript`).
// - The entry is named by its dispatch tag, the push before the redeem script (kob-wasm
//   `templates().entries`). A fill's size is its first argument, the 8-byte `nb` of `settle` or
//   `fill` (decode.ts `fillArg`).
// - The continuation is the state the covenant leaves. That is an amend's recorded state, the same
//   state, or `amountLeft - n` after an ask's fill. It counts only when its script is the output
//   that continues the covenant id. Otherwise the candidate search of records.ts `resolveRecord`
//   runs against the outputs of that transaction instead of the node.
//
// Only the plain ask and bid of either family are followed (`KobAsk`, `KobBid`, `KobAskKron`,
// `KobBidKron`), because they rest at a fixed price. Conditional orders, if-done entries and pair
// orders are not part of a standalone book.
//
// Every snapshot made here is a claim about the chain until the node confirms it, with
// `confirmSnapshotsOnNode` in kob/node-verify.ts. The transaction may never be accepted, for
// example in a block outside the selected chain or after a double spend, and its outputs may
// already be spent. A fill is a claim too. The caller counts it once one of the transaction's
// covenant outputs is seen on the node.
import type { NodeUtxo } from '../data/node';
import { bytesToHex, hexToBytes, parsePushes } from '../wallet/sigs';
import type { OrderSnapshot } from './cancel';
import { fillArg } from './decode';
import { outpointKey } from './node-verify';
import { custodiesOf, describeOrder, extensionOf, tokenCovIdOf } from './order-facts';
import { pinnedTemplateHash, resolveRecord, type PlacementRecord } from './records';
import type { Hex, OrderKind, OrderState, PayloadRecord, RecoveredOrder, TemplateInfo, TemplateName, TxJson } from './types';
import type { KobWasm } from './wasm';

/** The kinds a standalone book follows. */
export const STREAM_KINDS: readonly OrderKind[] = ['KobAsk', 'KobBid', 'KobAskKron', 'KobBidKron'];
/** True when `k` is a kind a standalone book follows. */
export const isStreamKind = (k: string): boolean => (STREAM_KINDS as readonly string[]).includes(k);
/** The entries that fill a followed order, an ask's `settle` with `nb > 0` and a bid's `fill`. */
const FILL_ENTRIES = new Set(['settle', 'fill']);

/** What one transaction's records say about orders. */
export interface TxFacts {
  txid: Hex;
  /** The outpoints its inputs spend, as `txid:index`. */
  spends: string[];
  /**
   * The orders its placement records create. Each record is verified alone, followed kinds only.
   */
  placements: RecoveredOrder[];
}

const isOrderRecord = (r: PayloadRecord): r is Extract<PayloadRecord, { type: 'order' }> => r.type === 'order';

/**
 * The KOB1 records of a transaction. Any other payload, or one kob-wasm cannot read, gives none.
 */
function recordsOf(kob: KobWasm, payload: Hex): PayloadRecord[] {
  if (!payload) return [];
  try {
    return kob.decodePayload(payload)?.records ?? [];
  } catch {
    return [];
  }
}

/** Reads the spends and placement records of one transaction. */
export function txFacts(kob: KobWasm, tx: TxJson): TxFacts {
  const placements: RecoveredOrder[] = [];
  for (const r of recordsOf(kob, tx.payload)) {
    if (!isOrderRecord(r) || !isStreamKind(r.template)) continue;
    try {
      // The indexer validates each record alone, because `recoverOrders` is all-or-nothing per
      // payload.
      const single = kob.encodePayload([r]);
      for (const ro of kob.recoverOrders({ ...tx, payload: single })) if (isStreamKind(ro.order.kind)) placements.push(ro);
    } catch {
      /* a record that does not verify creates nothing */
    }
  }
  return { txid: tx.id, spends: tx.inputs.map((i) => outpointKey(i.transactionId, i.index)), placements };
}

/**
 * Returns a placement as the snapshot it becomes once its transaction is accepted at block DAA
 * `daa`.
 */
export function snapshotOfPlacement(r: RecoveredOrder, daa: bigint): OrderSnapshot {
  const st = r.order as OrderState;
  const token = custodiesOf(st)[0]?.token ?? tokenCovIdOf(st);
  return {
    covenantId: r.covenantId,
    order: { transactionId: r.transactionId, index: r.output, amount: r.value, blockDaaScore: daa.toString(), covenantId: r.covenantId, state: st },
    custody: r.custody ? { transactionId: r.transactionId, index: r.custody.output, amount: r.custody.value, blockDaaScore: daa.toString(), covenantId: token, state: r.custody.state } : null,
    strays: [],
    refundDueDaa: null,
    deadline: r.deadline != null ? BigInt(r.deadline) : null,
    source: 'chain',
  };
}

/**
 * Returns the outpoints of a snapshot's order, custody and prefund UTXOs. Spending one changes the
 * order.
 */
export const outpointsOf = (s: OrderSnapshot): string[] => [
  outpointKey(s.order.transactionId, s.order.index),
  ...(s.custody ? [outpointKey(s.custody.transactionId, s.custody.index)] : []),
  ...(s.prefund ? [outpointKey(s.prefund.transactionId, s.prefund.index)] : []),
];

// --- Spends ---

/** An input that spends a followed order, as its signature script reveals it. */
export interface RevealedSpend {
  input: number;
  /** The state the spent order UTXO committed to. Its redeem script is kob-wasm's script of it. */
  state: OrderState;
  /** The entry its dispatch tag names, or null when the tag is none of the template's. */
  entry: string | null;
  /** Base units filled by a `settle` or `fill` with `nb > 0`, else null. */
  n: bigint | null;
}

const templateCache = new WeakMap<KobWasm, TemplateInfo[]>();
const followedTemplates = (kob: KobWasm): TemplateInfo[] => {
  let t = templateCache.get(kob);
  if (!t) templateCache.set(kob, (t = kob.templates().filter((x) => isStreamKind(x.name))));
  return t;
};

/** Reads input `i` of `tx` as a spend of a followed order, or returns null for any other input. */
export function revealSpend(kob: KobWasm, tx: TxJson, i: number): RevealedSpend | null {
  const sig = tx.inputs[i]?.signatureScript;
  if (!sig || sig.length < 8) return null;
  let pushes: Uint8Array[];
  try {
    pushes = parsePushes(hexToBytes(sig));
  } catch {
    return null;
  }
  if (pushes.length < 2) return null;
  const redeem = pushes[pushes.length - 1]!;
  const tag = bytesToHex(pushes[pushes.length - 2]!);
  for (const t of followedTemplates(kob)) {
    if (redeem.length !== t.prefixLen + t.stateLen + t.suffixLen) continue;
    let state: OrderState;
    try {
      state = kob.decodeState(t.name, bytesToHex(redeem.subarray(t.prefixLen, t.prefixLen + t.stateLen))) as OrderState;
      if (state.kind !== t.name || kob.redeemScript(state) !== bytesToHex(redeem)) continue;
    } catch {
      continue;
    }
    const entry = Object.entries(t.entries).find(([, v]) => v === tag)?.[0] ?? null;
    const nb = entry && FILL_ENTRIES.has(entry) && pushes.length >= 3 ? fillArg({ kind: 'bytes', value: bytesToHex(pushes[0]!) }) : null;
    return { input: i, state, entry, n: nb !== null && nb > 0n ? nb : null };
  }
  return null;
}

/**
 * A fill of a followed order, the input of the market data of standalone mode
 * (data/stream-market.ts).
 */
export interface StreamFill {
  txid: Hex;
  /** The filled order's covenant id when known, from the continuation or the followed order. */
  covenantId: Hex | null;
  token: Hex;
  /** `sell` when an ask was filled, so the aggressor bought. `buy` when a bid was filled. */
  side: 'sell' | 'buy';
  /** Base units filled. */
  amount: bigint;
  /** The order's limit in sompi per whole token, which is `scale` base units. */
  price: bigint;
  scale: bigint;
}

/**
 * Returns the fill a spend records, or null for a cancel or a refund.
 * An order with an auction also gives null, because its price at fill time is not its limit.
 */
export function fillOf(kob: KobWasm, s: RevealedSpend, txid: Hex, covenantId: Hex | null): StreamFill | null {
  if (s.n === null) return null;
  let d;
  try {
    d = describeOrder(s.state, kob);
  } catch {
    return null;
  }
  if (d.auction || d.price <= 0n || d.scale <= 0n) return null;
  return { txid, covenantId, token: d.tokenCovId, side: d.side, amount: s.n, price: d.price, scale: d.scale };
}

/**
 * Lists the outputs of `tx` as the UTXOs a node would return for them.
 * They are keyed by script, because the "address" `resolveRecord` asks for is the script itself.
 */
function localNode(tx: TxJson, daa: bigint): { getUtxosByAddresses(spks: string[]): Promise<NodeUtxo[]> } {
  const outs: NodeUtxo[] = tx.outputs.map((o, index) => ({
    address: o.scriptPublicKey, transactionId: tx.id, index, amount: o.value, scriptPublicKey: o.scriptPublicKey, blockDaaScore: daa.toString(), isCoinbase: false,
    covenantId: o.covenant?.covenantId ?? null,
  }));
  return {
    getUtxosByAddresses: async (spks) => {
      const want = new Set(spks);
      return outs.filter((u) => want.has(u.scriptPublicKey));
    },
  };
}

/**
 * How many candidates `resolveRecord` derives per continuation when the fill size is unknown.
 * Amounts walk every base unit up to this many, and beyond that they walk in minimum fills.
 */
const MAX_CANDIDATES = 2000;

/**
 * Finds where an order spent by a transaction lives on, from that transaction alone.
 * It returns null when the order ends there, meaning no output continues a covenant from the
 * order's input, or when the new state cannot be derived.
 *
 * `spend` is the revealed input. `known` is the snapshot the book holds for it, if any. Its
 * covenant id must be the continuation's, and its custody is kept when the transaction did not
 * spend it.
 *
 * The continuation is tried in this order:
 * - An amend's recorded state.
 * - The same state, which covers a bid's fill and a sweep.
 * - An ask's fill of `n`, which leaves `amountLeft - n`.
 * - The candidate continuations of the spent state, through `resolveRecord` over this
 *   transaction's outputs.
 *
 * The custody is the one the state implies, found among the outputs. The result is a claim, and the
 * node confirms it before the order is listed.
 */
export async function followSpend(kob: KobWasm, spend: RevealedSpend, tx: TxJson, daa: bigint, network: string, known?: OrderSnapshot | null): Promise<OrderSnapshot | null> {
  const bound = tx.outputs.map((o, index) => ({ o, index })).filter((x) => x.o.covenant && x.o.covenant.authorizingInput === spend.input);
  const out = known ? bound.find((x) => x.o.covenant!.covenantId === known.covenantId) : bound.length === 1 ? bound[0] : undefined;
  if (!out) return null;
  const cov = out.o.covenant!.covenantId;
  const st0 = spend.state;
  const spent = new Set(tx.inputs.map((i) => outpointKey(i.transactionId, i.index)));
  const keep = (u: OrderSnapshot['custody'] | undefined): OrderSnapshot['custody'] => (u && !spent.has(outpointKey(u.transactionId, u.index)) ? u : null);
  const base: OrderSnapshot = known ?? { covenantId: cov, order: { transactionId: tx.inputs[spend.input]!.transactionId, index: tx.inputs[spend.input]!.index, amount: '0', covenantId: cov, state: st0 }, custody: null, strays: [], refundDueDaa: null, deadline: null, source: 'chain' };
  const at = (state: OrderState, custody: OrderSnapshot['custody']): OrderSnapshot => ({
    ...base,
    covenantId: cov,
    order: { transactionId: tx.id, index: out.index, amount: out.o.value, blockDaaScore: daa.toString(), covenantId: cov, state },
    custody,
    strays: [],
    source: 'chain',
  });
  const spkOf = (st: OrderState): string | null => {
    try {
      return kob.scriptPublicKey(st);
    } catch {
      return null;
    }
  };
  // The continuation `state` with its custody. That is the one its state implies among this
  // transaction's outputs, else the one the transaction did not spend.
  const withCustody = async (state: OrderState): Promise<OrderSnapshot | null> => {
    const r = await resolveIn(kob, tx, daa, network, cov, state);
    if (!r) return null;
    return at(state, r.custody ?? keep(known?.custody));
  };
  // 1. An in-place amend. The record names the new state, which counts only when its script is the
  //    continuation's.
  const amend = recordsOf(kob, tx.payload).find((r): r is Extract<PayloadRecord, { type: 'amend' }> => r.type === 'amend' && r.input === spend.input && r.output === out.index);
  if (amend) {
    try {
      const st = kob.decodeState(amend.template as TemplateName, amend.state) as OrderState;
      if (st.kind !== st0.kind || spkOf(st) !== out.o.scriptPublicKey) return null;
      return at(st, keep(known?.custody));
    } catch {
      return null;
    }
  }
  // 2. The same state. A bid's fill keeps its state and spends from its escrow, and a sweep stays
  // in place.
  if (spkOf(st0) === out.o.scriptPublicKey) return keep(known?.custody) ? at(st0, keep(known?.custody)) : withCustody(st0);
  // 3. An ask's fill of n. The covenant leaves amountLeft - n.
  const s = st0.state as unknown as Record<string, string | undefined>;
  if (spend.n !== null && s.amountLeft !== undefined) {
    const left = BigInt(s.amountLeft) - spend.n;
    if (left > 0n) {
      const next = { kind: st0.kind, state: { ...st0.state, amountLeft: left.toString() } } as OrderState;
      if (spkOf(next) === out.o.scriptPublicKey) return withCustody(next);
    }
  }
  // 4. Any other continuation. Match the candidates of the spent state against this transaction's
  // outputs.
  const r = await resolveIn(kob, tx, daa, network, cov, st0, true);
  if (!r || !r.order || r.order.transactionId !== tx.id || r.order.index !== out.index) return null;
  return at(r.order.state, r.custody ?? keep(known?.custody));
}

/**
 * Runs `resolveRecord` for a record of `state` against the outputs of `tx`, without asking a node.
 * With `search` it also walks the candidate continuations.
 */
async function resolveIn(kob: KobWasm, tx: TxJson, daa: bigint, network: string, cov: Hex, state: OrderState, search = false) {
  const kind = state.kind as OrderKind;
  const templateHash = pinnedTemplateHash(kob, kind);
  if (!templateHash) return null;
  const record: PlacementRecord = {
    version: 1, network, maker: state.state.maker, txid: null, output: null, covenantId: cov, kind, templateHash, state: kob.encodeState(state), amount: null, custody: null,
    value: '0', placedAtUnix: '0', placedAtDaa: daa.toString(), ext: extensionOf(state) ?? null, last: null,
  };
  try {
    const r = await resolveRecord(kob, localNode(tx, daa), record, (spk) => spk, { maxCandidates: search ? MAX_CANDIDATES : 0, chunk: MAX_CANDIDATES });
    if (r.status !== 'live' || !r.order) return null;
    return r;
  } catch {
    return null;
  }
}

