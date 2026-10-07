// Node reconciliation of input facts.
//
// The maker's SIGHASH_ALL only commits to the KAS amount and script of the inputs the WALLET signs. Every covenant / token input (custody UTXO,
// strays) is authorised by its script, so nothing but the chain itself ties its KAS value to the transaction. The indexer is not an authority
// for those values: every input (and every snapshot UTXO before a cancel / amend is built) is re-read from the NODE, by address and outpoint,
// and the app takes amount, script and covenant id from there. A UTXO the node does not know, or knows with another script / covenant id,
// blocks the action.
import type { NodeApi } from '../data/node';
import type { OrderSnapshot } from './cancel';
import { SnapshotError, ownTokenFacts } from './cancel';
import { custodiesOf } from './order-facts';
import type { Hex, TokenProgram, TokenState, TokenUtxo, TxInputJson } from './types';
import type { KobWasm } from './wasm';

export interface NodeInputFact {
  amount: bigint;
  scriptPublicKey: string;
  covenantId: Hex | null;
}
/** node-confirmed facts by outpoint (`txid:index`) */
export type NodeInputFacts = ReadonlyMap<string, NodeInputFact>;

export const outpointKey = (txid: string, index: number): string => `${txid}:${index}`;

export interface OutpointRef {
  transactionId: Hex;
  index: number;
  scriptPublicKey: string;
}

/** Reads the given outpoints from the node (by the address of their script). Outpoints the node does not return are simply absent from the result. */
export async function fetchNodeInputFacts(
  node: Pick<NodeApi, 'getUtxosByAddresses'>,
  refs: readonly OutpointRef[],
  spkToAddress: (spk: string) => string,
): Promise<Map<string, NodeInputFact>> {
  const facts = new Map<string, NodeInputFact>();
  if (!refs.length) return facts;
  const wanted = new Set(refs.map((r) => outpointKey(r.transactionId, r.index)));
  const addresses = [...new Set(refs.map((r) => spkToAddress(r.scriptPublicKey)))];
  for (const u of await node.getUtxosByAddresses(addresses)) {
    const key = outpointKey(u.transactionId, u.index);
    if (wanted.has(key)) facts.set(key, { amount: BigInt(u.amount), scriptPublicKey: u.scriptPublicKey, covenantId: u.covenantId ?? null });
  }
  return facts;
}

export interface InputMismatch {
  index: number;
  reason: 'missing' | 'amount' | 'script' | 'covenant';
  /** human-readable detail (claimed vs node) */
  detail: string;
}

/** Compares the inputs of a transaction with node facts. Every input must be present and equal in amount, script and covenant id. */
export function inputMismatches(inputs: readonly TxInputJson[], facts: NodeInputFacts): InputMismatch[] {
  const out: InputMismatch[] = [];
  inputs.forEach((inp, index) => {
    const f = facts.get(outpointKey(inp.transactionId, inp.index));
    if (!f) return void out.push({ index, reason: 'missing', detail: 'the node does not know this UTXO (spent, unconfirmed or invented)' });
    if (f.amount !== BigInt(inp.utxo.amount)) return void out.push({ index, reason: 'amount', detail: `claimed ${inp.utxo.amount} sompi, the node holds ${f.amount}` });
    if (f.scriptPublicKey !== inp.utxo.scriptPublicKey) return void out.push({ index, reason: 'script', detail: 'the node holds another script on this outpoint' });
    if ((f.covenantId ?? null) !== (inp.utxo.covenantId ?? null)) return void out.push({ index, reason: 'covenant', detail: 'the node holds another covenant id on this outpoint' });
  });
  return out;
}

/** Fetches the facts of every input of a transaction and returns them with the mismatches. Throws only when the node cannot be asked. */
export async function confirmInputsOnNode(
  node: Pick<NodeApi, 'getUtxosByAddresses'>,
  inputs: readonly TxInputJson[],
  spkToAddress: (spk: string) => string,
): Promise<{ facts: Map<string, NodeInputFact>; mismatches: InputMismatch[] }> {
  const facts = await fetchNodeInputFacts(node, inputs.map((i) => ({ transactionId: i.transactionId, index: i.index, scriptPublicKey: i.utxo.scriptPublicKey })), spkToAddress);
  return { facts, mismatches: inputMismatches(inputs, facts) };
}

// ------------------------------------------------------------------------------------------------ snapshots

function tokenSpk(kob: KobWasm, tplHash: Hex, state: TokenState): string | null {
  const tpl = kob.templates().find((t) => t.hash === tplHash && t.tokenSlots);
  if (!tpl) return null;
  try {
    return kob.tokenScriptPublicKey(tpl.name as TokenProgram, state);
  } catch {
    return null;
  }
}

/**
 * Re-reads the order UTXO, its custody UTXO(s) and its strays from the node and returns a snapshot whose amounts are the node's. Fails
 * (SnapshotError `node-mismatch`) when the order or a custody (a pair order: of A or of B, a sell-first entry's prefund too) is not on the node
 * with the script derived from its state (on its token's program) and the expected token covenant id; a stray the node does not hold in that
 * form is dropped (spent or invented: spending it would invalidate the transaction). A pair order's strays may be of either of its tokens.
 * A snapshot whose amounts came from anywhere but the node must never reach a builder.
 */
export async function confirmSnapshotOnNode(
  kob: KobWasm,
  node: Pick<NodeApi, 'getUtxosByAddresses'>,
  snap: OrderSnapshot,
  spkToAddress: (spk: string) => string,
): Promise<OrderSnapshot> {
  const st = snap.order.state;
  const own = ownTokenFacts(st);
  const tplOf = (token: Hex | null | undefined): Hex => own.find((t) => t.covenantId === token)?.tplHash ?? own[0]!.tplHash;
  const cs = custodiesOf(st);
  const orderSpk = kob.scriptPublicKey(st);
  // each custody on its own token's program (the first: custodies[0], the prefund: custodies[1])
  const custodyToken = cs[0]?.token ?? own[0]!.covenantId;
  const prefundToken = cs[1]?.token ?? null;
  const custodySpk = snap.custody ? tokenSpk(kob, tplOf(custodyToken), snap.custody.state) : null;
  const prefundSpk = snap.prefund && prefundToken ? tokenSpk(kob, tplOf(prefundToken), snap.prefund.state) : null;
  // the order's covenant id may own strays of any of its own tokens (a pair order: A and B): each checked against its token's program
  const strayToken = (s: TokenUtxo): { tpl: Hex; cov: Hex } => {
    const t = own.find((x) => x.covenantId === s.covenantId) ?? own[0]!;
    return { tpl: t.tplHash, cov: t.covenantId };
  };
  const straySpks = snap.strays.map((s) => tokenSpk(kob, strayToken(s).tpl, s.state));
  const refs: OutpointRef[] = [{ transactionId: snap.order.transactionId, index: snap.order.index, scriptPublicKey: orderSpk }];
  if (snap.custody && custodySpk) refs.push({ transactionId: snap.custody.transactionId, index: snap.custody.index, scriptPublicKey: custodySpk });
  if (snap.prefund && prefundSpk) refs.push({ transactionId: snap.prefund.transactionId, index: snap.prefund.index, scriptPublicKey: prefundSpk });
  snap.strays.forEach((s, i) => {
    const spk = straySpks[i];
    if (spk) refs.push({ transactionId: s.transactionId, index: s.index, scriptPublicKey: spk });
  });
  // foreign strays (other tokens owned by the order id): each checked against its OWN program (the indexer's proven one) and token covenant id
  const programSpk = (program: TokenProgram, state: TokenState): string | null => {
    try {
      return kob.tokenScriptPublicKey(program, state);
    } catch {
      return null;
    }
  };
  const foreignSpks = (snap.foreign ?? []).map((g) => g.utxos.map((u) => programSpk(g.token.program, u.state)));
  (snap.foreign ?? []).forEach((g, k) => g.utxos.forEach((u, i) => {
    const spk = foreignSpks[k]![i];
    if (spk) refs.push({ transactionId: u.transactionId, index: u.index, scriptPublicKey: spk });
  }));
  const facts = await fetchNodeInputFacts(node, refs, spkToAddress);
  const confirmed = (u: { transactionId: Hex; index: number }, spk: string | null, covenantId: Hex): NodeInputFact | null => {
    const f = facts.get(outpointKey(u.transactionId, u.index));
    return f && spk !== null && f.scriptPublicKey === spk && f.covenantId === covenantId ? f : null;
  };
  const orderFact = confirmed(snap.order, orderSpk, snap.covenantId);
  if (!orderFact) throw new SnapshotError('node-mismatch', `order ${snap.covenantId}: its current UTXO is not on the node with the claimed script and covenant id`);
  let custody: TokenUtxo | null = null;
  if (snap.custody) {
    const cf = confirmed(snap.custody, custodySpk, custodyToken);
    if (!cf) throw new SnapshotError('node-mismatch', `order ${snap.covenantId}: its custody token UTXO is not on the node with the claimed script and token covenant id`);
    custody = { ...snap.custody, amount: cf.amount.toString(), covenantId: custodyToken };
  }
  let prefund: TokenUtxo | null = null;
  if (snap.prefund) {
    const pf = prefundToken ? confirmed(snap.prefund, prefundSpk, prefundToken) : null;
    if (!pf || !prefundToken) throw new SnapshotError('node-mismatch', `order ${snap.covenantId}: its prefund custody UTXO is not on the node with the claimed script and token covenant id`);
    prefund = { ...snap.prefund, amount: pf.amount.toString(), covenantId: prefundToken };
  }
  const strays: TokenUtxo[] = [];
  snap.strays.forEach((s, i) => {
    const cov = strayToken(s).cov;
    const f = confirmed(s, straySpks[i], cov);
    if (f) strays.push({ ...s, amount: f.amount.toString(), covenantId: cov });
  });
  const foreign = (snap.foreign ?? [])
    .map((g, k) => ({
      token: g.token,
      utxos: g.utxos.flatMap((u, i) => {
        const f = confirmed(u, foreignSpks[k]![i] ?? null, g.token.covenantId);
        return f ? [{ ...u, amount: f.amount.toString(), covenantId: g.token.covenantId }] : [];
      }),
    }))
    .filter((g) => g.utxos.length > 0);
  return { ...snap, order: { ...snap.order, amount: orderFact.amount.toString() }, custody, ...(snap.prefund ? { prefund } : {}), strays, ...(snap.foreign ? { foreign } : {}) };
}
