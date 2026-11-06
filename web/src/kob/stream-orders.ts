// What a block's transaction does to the order book, read from the chain alone (standalone mode, data/standalone.ts). Nothing here is protocol logic
// of its own:
//   * placements are re-derived with kob-wasm `recoverOrders`, record by record as the indexer does (crates/kobd/src/indexer/processor.rs:
//     each ORDER record of a payload alone, so one bad record does not hide the others);
//   * a spend of an order is read from its input's signature script as a chain observer reads it (the indexer's `parse_reveal`): the redeem script
//     (the last push) is accepted as an order of a template this build pins only when kob-wasm re-derives exactly that script from the state span it
//     holds (`decodeState` then `redeemScript`); the entry is named by its dispatch tag (the push before it, kob-wasm `templates().entries`); a fill's
//     size is its first argument (decode.ts `fillArg`, the 8-byte `nb` of `settle` / `fill`);
//   * the continuation is the state the covenant leaves (an amend's recorded state, the same state, or `amountLeft - n` of an ask's fill), accepted
//     only when its script is the output that continues the covenant id; else the candidate search a maker's receipt is resolved with (records.ts
//     `resolveRecord`), against the outputs of that transaction instead of the node.
//
// Only the KAS-book kinds of the indexer's `/v1/books` that can rest at a fixed price are followed: the plain ask and bid of either family
// (`KobAsk`, `KobBid`, `KobAskKron`, `KobBidKron`). Conditional orders, if-done entries and pair orders are not part of a standalone book.
//
// Every snapshot made here is a CLAIM about the chain until the node confirms it (kob/node-verify.ts `confirmSnapshotsOnNode`, the check the book
// gets before an order is taken): the transaction may never be accepted (a block outside the selected chain, a double spend), or its outputs may be
// spent already. A fill is a claim too: the caller counts it once one of the transaction's covenant outputs is seen on the node.
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
export const isStreamKind = (k: string): boolean => (STREAM_KINDS as readonly string[]).includes(k);
/** The entries that fill a followed order (an ask's `settle` with `nb > 0`, a bid's `fill`); `settle` with `nb = 0` is a refund. */
const FILL_ENTRIES = new Set(['settle', 'fill']);

/** What one transaction's records say about orders. */
export interface TxFacts {
  txid: Hex;
  /** the outpoints its inputs spend (`txid:index`) */
  spends: string[];
  /** orders its placement records create, each record verified on its own (of the followed kinds only) */
  placements: RecoveredOrder[];
}

const isOrderRecord = (r: PayloadRecord): r is Extract<PayloadRecord, { type: 'order' }> => r.type === 'order';

/** The KOB1 records of a transaction (none for any other payload, or one kob-wasm cannot read). */
function recordsOf(kob: KobWasm, payload: Hex): PayloadRecord[] {
  if (!payload) return [];
  try {
    return kob.decodePayload(payload)?.records ?? [];
  } catch {
    return [];
  }
}

/** Reads the placement records of one transaction (pure given kob). */
export function txFacts(kob: KobWasm, tx: TxJson): TxFacts {
  const placements: RecoveredOrder[] = [];
  for (const r of recordsOf(kob, tx.payload)) {
    if (!isOrderRecord(r) || !isStreamKind(r.template)) continue;
    try {
      // the record alone, as the indexer validates it: `recoverOrders` is all-or-nothing per payload
      const single = kob.encodePayload([r]);
      for (const ro of kob.recoverOrders({ ...tx, payload: single })) if (isStreamKind(ro.order.kind)) placements.push(ro);
    } catch {
      /* a record that does not verify creates nothing */
    }
  }
  return { txid: tx.id, spends: tx.inputs.map((i) => outpointKey(i.transactionId, i.index)), placements };
}

/** A placement as the snapshot it becomes once its transaction is accepted (block DAA `daa`). */
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

/** The outpoints a snapshot holds (its order and custody UTXOs): a transaction spending one of them changes the order. */
export const outpointsOf = (s: OrderSnapshot): string[] => [
  outpointKey(s.order.transactionId, s.order.index),
  ...(s.custody ? [outpointKey(s.custody.transactionId, s.custody.index)] : []),
  ...(s.prefund ? [outpointKey(s.prefund.transactionId, s.prefund.index)] : []),
];

// ------------------------------------------------------------------------------------------------ spends

/** An input that spends a followed order, as its signature script reveals it. */
export interface RevealedSpend {
  input: number;
  /** the state the spent order UTXO committed to (its redeem script is exactly kob-wasm's script of this state) */
  state: OrderState;
  /** the entry its dispatch tag names (`settle`, `fill`, `cancel`, `refund`), null when the tag is none of the template's */
  entry: string | null;
  /** base units filled (a `settle` / `fill` with `nb > 0`), else null */
  n: bigint | null;
}

const templateCache = new WeakMap<KobWasm, TemplateInfo[]>();
const followedTemplates = (kob: KobWasm): TemplateInfo[] => {
  let t = templateCache.get(kob);
  if (!t) templateCache.set(kob, (t = kob.templates().filter((x) => isStreamKind(x.name))));
  return t;
};

/** Reads input `i` of `tx` as a spend of a followed order; null for any other input. */
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

/** A fill of a followed order: what the market data of standalone mode is built from (data/stream-market.ts). */
export interface StreamFill {
  txid: Hex;
  /** the filled order's covenant id when known (the continuation's, or the followed order's) */
  covenantId: Hex | null;
  token: Hex;
  /** `sell`: an ask was filled (the aggressor bought); `buy`: a bid was filled */
  side: 'sell' | 'buy';
  /** base units filled */
  amount: bigint;
  /** sompi per whole token (`scale` base units): the order's limit */
  price: bigint;
  scale: bigint;
}

/** The fill a spend records (null for a cancel, a refund, or an order with an auction: its price at fill time is not its limit). */
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

/** The outputs of `tx` as the UTXOs a node would list for them, keyed by their script (the "address" `resolveRecord` asks for is the script itself). */
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

/** Candidates `resolveRecord` derives per continuation when the fill size is not known (amounts walk every base unit up to this, else in minimum fills). */
const MAX_CANDIDATES = 2000;

/**
 * Where an order a transaction spends lives on, from that transaction alone: null when it ends there (no output continues a covenant from the order's
 * input) or when its new state cannot be derived. `spend` is the revealed input; `known` the snapshot the book holds for it, if any (its covenant id
 * must be the continuation's, and its custody is kept when the transaction did not spend it). The continuation, in this order: an amend's recorded
 * state; the same state (a bid's fill, a sweep); an ask's fill of `n`, `amountLeft - n`; else the candidate continuations of the spent state
 * (`resolveRecord` over this transaction's outputs). The custody is the one the state implies, found among the outputs. The result is a claim: the
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
  // the continuation `state` with its custody: the one its state implies among this transaction's outputs, else the one the transaction did not spend
  const withCustody = async (state: OrderState): Promise<OrderSnapshot | null> => {
    const r = await resolveIn(kob, tx, daa, network, cov, state);
    if (!r) return null;
    return at(state, r.custody ?? keep(known?.custody));
  };
  // 1. an in-place amend: the record names the new state; it counts only when its script is the continuation's
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
  // 2. the same state (a bid's fill keeps its state and spends from its escrow; a sweep in place)
  if (spkOf(st0) === out.o.scriptPublicKey) return keep(known?.custody) ? at(st0, keep(known?.custody)) : withCustody(st0);
  // 3. an ask's fill of n: the covenant leaves amountLeft - n
  const s = st0.state as unknown as Record<string, string | undefined>;
  if (spend.n !== null && s.amountLeft !== undefined) {
    const left = BigInt(s.amountLeft) - spend.n;
    if (left > 0n) {
      const next = { kind: st0.kind, state: { ...st0.state, amountLeft: left.toString() } } as OrderState;
      if (spkOf(next) === out.o.scriptPublicKey) return withCustody(next);
    }
  }
  // 4. any other continuation: the candidates of the spent state, matched against this transaction's outputs
  const r = await resolveIn(kob, tx, daa, network, cov, st0, true);
  if (!r || !r.order || r.order.transactionId !== tx.id || r.order.index !== out.index) return null;
  return at(r.order.state, r.custody ?? keep(known?.custody));
}

/** `resolveRecord` of a record whose state is `state`, against the outputs of `tx` (no node is asked); `search`: walk the candidate continuations too. */
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

