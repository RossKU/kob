// Test fixtures shared by the decode / cancel / records / balances / positions tests (node only: reads the golden vectors from disk).
//
// The strongest oracle available offline is kob-wasm itself: a request built by `kob.build`, signed with a local key, finalized and passed
// through `kob.validate` is consensus-valid (the script engine runs the covenants). The fixtures therefore take the golden requests of
// crates/kob-protocol/vectors/golden.json, re-key them to a test key we hold the secret of, and build real transactions from them.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { NodeUtxo } from '../data/node';
import type { OrderView, TokenUtxoView } from '../data/indexer-types';
import type { OrderSnapshot } from '../kob/cancel';
import type {
  ActionRequest, AnyState, BuiltTx, Hex, KeyUtxo, Kcc20State, OrderState, RecoveredOrder, SignedTx, TokenUtxo, TxInputJson, TxJson,
} from '../kob/types';
import { extensionOfState } from '../kob/token-state';
import { custodiesOf, tokenCovIdOf, tokenTplHashOf } from '../kob/order-facts';
import type { NodeInputFact } from '../kob/node-verify';
import type { KobWasm } from '../kob/wasm';
import { pubkeyOf, signBuilt } from './local-signer';

// ------------------------------------------------------------------------------------------------ keys and constants

export const MAKER_SK = '11'.repeat(32);
export const OTHER_SK = '22'.repeat(32);
export const KEEPER_SK = '33'.repeat(32);
export const MAKER = { sk: MAKER_SK, pk: pubkeyOf(MAKER_SK) };
export const OTHER = { sk: OTHER_SK, pk: pubkeyOf(OTHER_SK) };
export const KEEPER = { sk: KEEPER_SK, pk: pubkeyOf(KEEPER_SK) };

/** The token every golden request trades: a KCC20Ref (3/3) token with a fixed-supply extension commitment. */
export const TOKEN = {
  covenantId: '70'.repeat(32),
  program: 'KCC20Ref' as const,
  templateHash: 'f4ac029d2c3c74dd3dcaeb64245f7d0a0977e27c2956f3977540a11dc7c45b1f',
  ext: 'ee'.repeat(32),
  prefixLen: 1,
  suffixLen: 2977,
  decimals: 3,
  ticker: 'TST',
};
export const ZERO32 = '00'.repeat(32);

// ------------------------------------------------------------------------------------------------ golden vectors

interface GoldenTx { name: string; request: ActionRequest & Record<string, unknown>; built: BuiltTx; signatures: unknown; finalize: unknown; signed: SignedTx }
interface Golden { transactions: GoldenTx[] }
let goldenCache: Golden | null = null;

export function golden(): Golden {
  goldenCache ??= JSON.parse(readFileSync(fileURLToPath(new URL('../../../crates/kob-protocol/vectors/golden.json', import.meta.url)), 'utf8')) as Golden;
  return goldenCache;
}
export function goldenTx(name: string): GoldenTx {
  const t = golden().transactions.find((x) => x.name === name);
  if (!t) throw new Error(`no golden transaction ${name}`);
  return t;
}

/** Keys that play "the maker" in a golden request: every `maker`, every funding `pubkey`, every token state owned by a P2PK key. */
function makerKeysOf(node: unknown, acc = new Set<string>()): Set<string> {
  if (Array.isArray(node)) node.forEach((n) => makerKeysOf(n, acc));
  else if (node && typeof node === 'object') {
    const o = node as Record<string, unknown>;
    if (typeof o.maker === 'string') acc.add(o.maker);
    if (Array.isArray(o.funding)) for (const f of o.funding as { pubkey?: string }[]) if (f.pubkey) acc.add(f.pubkey);
    if (typeof o.owner === 'string' && (o.owner_scheme === 0 || o.id_type === 3)) acc.add(o.owner);
    Object.values(o).forEach((v) => makerKeysOf(v, acc));
  }
  return acc;
}

/** Golden request re-keyed to `maker` (every maker-like key, also inside committed exit states, is replaced). */
export function goldenRequest<T = ActionRequest>(name: string, maker: Hex = MAKER.pk): T {
  const req = goldenTx(name).request;
  let text = JSON.stringify(req);
  for (const k of makerKeysOf(req)) text = text.split(k).join(maker);
  return JSON.parse(text) as T;
}

// ------------------------------------------------------------------------------------------------ building and signing

/** Signs with local keys, finalizes with tightened budgets and validates in the script engine (throws when consensus-invalid). */
export function signAndValidate(kob: KobWasm, built: BuiltTx, keys: Hex[] = [MAKER.sk]): SignedTx {
  const signed = kob.finalize(built, signBuilt(built, keys), { tightenBudgets: true });
  kob.validate(signed);
  return signed;
}

export interface Placed {
  request: ActionRequest;
  built: BuiltTx;
  signed: SignedTx;
  txid: Hex;
  recovered: RecoveredOrder[];
  snapshots: OrderSnapshot[];
}

/** Builds a golden create request for the test maker, signs it, validates it, and derives the live snapshots of the orders it created. */
export function placeGolden(kob: KobWasm, name: string, maker = MAKER, daa = 1000n): Placed {
  const request = goldenRequest<ActionRequest>(name, maker.pk);
  const built = kob.build(request);
  const signed = signAndValidate(kob, built, [maker.sk]);
  const recovered = kob.recoverOrders(signed.tx);
  return { request, built, signed, txid: signed.tx.id, recovered, snapshots: recovered.map((r) => snapshotOfRecovered(r, daa)) };
}

/** A recovered order as the live snapshot it becomes once the tx is accepted at `daa` (a pair order: each custody of its own token, A or B). */
export function snapshotOfRecovered(r: RecoveredOrder, daa = 1000n): OrderSnapshot {
  const st = r.order as OrderState;
  const cs = custodiesOf(st);
  const part = (c: RecoveredOrder['custody'] | undefined, k: number): TokenUtxo | null =>
    c ? { transactionId: r.transactionId, index: c.output, amount: c.value, blockDaaScore: daa.toString(), covenantId: cs[k]?.token ?? tokenCovIdOf(st), state: c.state } : null;
  const custody = part(r.custody, 0);
  const prefund = part(r.prefund, 1);
  return {
    covenantId: r.covenantId,
    order: { transactionId: r.transactionId, index: r.output, amount: r.value, blockDaaScore: daa.toString(), covenantId: r.covenantId, state: st },
    custody,
    ...(prefund ? { prefund } : {}),
    strays: [],
    refundDueDaa: null,
    deadline: r.deadline != null ? BigInt(r.deadline) : null,
    source: 'chain',
  };
}

/** A stray: tokens sent to an order's covenant id from outside (owner scheme 0x04 owned by the order). */
export function strayFor(snap: OrderSnapshot, amount: bigint, n = 90, carrier = 1_000_000_000n): TokenUtxo {
  const ext = (snap.custody ? extensionOfState(snap.custody.state) : null) ?? TOKEN.ext;
  return {
    transactionId: n.toString(16).padStart(2, '0').repeat(32), index: n, amount: carrier.toString(), blockDaaScore: '1200',
    covenantId: tokenCovIdOf(snap.order.state),
    state: { amount: amount.toString(), owner: snap.covenantId, owner_scheme: 4, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: ext },
  };
}

export function keyUtxo(pubkey: Hex, sompi: bigint, n = 200): KeyUtxo {
  return { transactionId: n.toString(16).padStart(2, '0').repeat(32), index: n, amount: sompi.toString(), blockDaaScore: '500', covenantId: null, pubkey };
}

export function tokenState(amount: bigint, owner: Hex, scheme = 0, ext: Hex = TOKEN.ext): Kcc20State {
  return { amount: amount.toString(), owner, owner_scheme: scheme, borrow_scheme: 0, borrow_guard: ZERO32, extension_commitment: ext };
}

export function tokenUtxo(amount: bigint, owner: Hex, n = 150, carrier = 1_000_000_000n, scheme = 0, ext: Hex = TOKEN.ext): TokenUtxo {
  return {
    transactionId: n.toString(16).padStart(2, '0').repeat(32), index: n, amount: carrier.toString(), blockDaaScore: '500', covenantId: TOKEN.covenantId,
    state: tokenState(amount, owner, scheme, ext),
  };
}

// ------------------------------------------------------------------------------------------------ a tiny fake node

/** Address stand-in used by tests: the spk string itself (records.ts takes `spkToAddress` as a parameter). */
export const testAddress = (spk: string): string => `test:${spk}`;

/** In-memory UTXO set with the `getUtxosByAddresses` of `data/node.ts`; `applyTx` spends inputs and adds outputs like a chain would. */
export class FakeChain {
  private utxos = new Map<string, NodeUtxo>();
  calls: string[][] = [];
  private key = (txid: string, i: number) => `${txid}:${i}`;

  add(u: { transactionId: Hex; index: number; amount: string; scriptPublicKey: string; covenantId?: Hex | null; blockDaaScore?: string }): void {
    this.utxos.set(this.key(u.transactionId, u.index), {
      address: testAddress(u.scriptPublicKey), transactionId: u.transactionId, index: u.index, amount: u.amount, scriptPublicKey: u.scriptPublicKey,
      blockDaaScore: u.blockDaaScore ?? '1000', isCoinbase: false, covenantId: u.covenantId ?? null,
    });
  }
  applyTx(tx: TxJson, daa = '1000'): void {
    for (const i of tx.inputs) this.utxos.delete(this.key(i.transactionId, i.index));
    tx.outputs.forEach((o, idx) =>
      this.add({ transactionId: tx.id, index: idx, amount: o.value, scriptPublicKey: o.scriptPublicKey, covenantId: o.covenant?.covenantId ?? null, blockDaaScore: daa }),
    );
  }
  has(txid: string, i: number): boolean {
    return this.utxos.has(this.key(txid, i));
  }
  async getUtxosByAddresses(addresses: string[]): Promise<NodeUtxo[]> {
    this.calls.push(addresses);
    const want = new Set(addresses);
    return [...this.utxos.values()].filter((u) => want.has(u.address));
  }
}

// ------------------------------------------------------------------------------------------------ indexer views

export function tokenUtxoView(t: TokenUtxo, role: 'custody' | 'stray' | 'owned', extra: Partial<TokenUtxoView> = {}): TokenUtxoView {
  return {
    txid: t.transactionId, index: t.index, token: TOKEN.covenantId, owner: t.state.owner, amount: t.state.amount, value: t.amount, role,
    created_daa: Number(t.blockDaaScore ?? 0), spent: false, spent_txid: null, state: t.state, confirmations: 100, settled: true, ...extra,
  };
}

/** A well-formed indexer `OrderView` for a snapshot (open order, current state proven). Override any field with `over`. */
export function orderViewOf(kob: KobWasm, snap: OrderSnapshot, over: Partial<OrderView> = {}): OrderView {
  const st = snap.order.state;
  const s = st.state as unknown as Record<string, string>;
  const tpl = kob.templates().find((t) => t.name === st.kind)!;
  const asks = st.kind === 'KobAsk' || st.kind === 'KobCondAsk' || st.kind === 'KobIfdAsk';
  const left = 'amountLeft' in s ? String(s.amountLeft) : null;
  const view: OrderView = {
    covenant_id: snap.covenantId, contract: st.kind, template_hash: tpl.hash, family: 1, side: asks || s.side === '1' ? 1 : 2, maker: s.maker, token: tokenCovIdOf(st),
    token_template_hash: tokenTplHashOf(st), extension_commitment: (snap.custody ? extensionOfState(snap.custody.state) : null) ?? s.extensionCommitment ?? TOKEN.ext,
    scale: Number(s.scale), min_fill: s.minFill ?? null, price: s.price ?? s.tpPrice ?? null, tip: s.tip, tif: s.tif != null ? Number(s.tif) : null,
    expiry_daa: Number(s.expiryDaa), active_from: Number(s.activeFrom), in_book: true, budget_rate: null, reserve: s.reserve ?? null, initial_amount: left,
    listed: true, unlisted_reason: null, origin: 'placement', parent: null,
    genesis: { txid: snap.order.transactionId, out: snap.order.index, block_seq: 1, daa: Number(snap.order.blockDaaScore ?? 0), confirmations: 100, settled: true },
    status: 'open', filled_amount: '0', amount_left: left, amount_estimated: false,
    current: { txid: snap.order.transactionId, index: snap.order.index, value: snap.order.amount }, state_known: true, state: st as AnyState,
    current_daa: Number(snap.order.blockDaaScore ?? 0), deadline: snap.deadline != null ? Number(snap.deadline) : null, refund_due_daa: snap.refundDueDaa != null ? Number(snap.refundDueDaa) : null,
    custody: snap.custody ? { expected_amount: snap.custody.state.amount, utxo: tokenUtxoView(snap.custody, 'custody'), ok: true } : { expected_amount: null, utxo: null, ok: true },
    strays: snap.strays.map((x) => tokenUtxoView(x, 'stray')),
    last_block: 1, last_daa: Number(snap.order.blockDaaScore ?? 0), confirmations: 100, settled: true, expired: false, children: [],
  };
  return { ...view, ...over };
}

// ------------------------------------------------------------------------------------------------ registry fixtures

const repoFile = (rel: string): string => readFileSync(fileURLToPath(new URL(`../../../${rel}`, import.meta.url)), 'utf8');
export const shippedRegistryJson = (): string => repoFile('registry/tokens.json');
export const exampleRegistryJson = (): string => repoFile('registry/tokens.example.json');

/** The registry fields an `official` token needs besides `verified` and `listed`: a clean genesis check and a genesis record that found no live mint authority. */
export const officialGenesis = () => ({
  genesis_verified: true,
  genesis: { txid: 'ab'.repeat(32), daa_score: 10, outputs: [1, 2], supply: 1000, minter_outputs: [], live_minters: [], checked_at_daa: 20, source: 'test' },
});

/**
 * A registry in which the golden token is TRADABLE: the example's templates with the two KCC-20 programs marked `reviewed`, and two listed,
 * verified tokens (TST on the 3/3 reference program = the golden token, EIGHT on the 8/8 program).
 */
export function tradableRegistryJson(): Record<string, any> {
  const doc = JSON.parse(exampleRegistryJson());
  for (const t of doc.templates) if (t.family === 'kcc20') t.review_status = 'reviewed';
  const listed = {
    family: 'kcc20', extension_commitment: TOKEN.ext, extension_class: 'fixed-supply-standard', lot_size: 1000, tick: 100, status: 'listed', verified: true,
  };
  doc.tokens = [
    { ticker: TOKEN.ticker, name: 'Test token', covenant_id: TOKEN.covenantId, template_id: 'kcc20-ref-3x3', decimals: TOKEN.decimals, ...listed },
    { ticker: 'EIGHT', name: 'Eight slot token', covenant_id: '80'.repeat(32), template_id: 'kcc20-ref-8x8', decimals: 8, ...listed },
  ];
  return doc;
}

/** Node facts that agree with the inputs of a built tx (a node that confirms everything): for decoder tests that are not about node reconciliation. */
export function nodeFactsOf(built: { tx: { inputs: TxInputJson[] } }): Map<string, NodeInputFact> {
  return new Map(
    built.tx.inputs.map((i) => [`${i.transactionId}:${i.index}`, { amount: BigInt(i.utxo.amount), scriptPublicKey: i.utxo.scriptPublicKey, covenantId: i.utxo.covenantId ?? null }]),
  );
}
