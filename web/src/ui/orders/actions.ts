// Planning of the order actions: cancel, cancel-all (position, token or everything), cancel-replace
// (amend) and refund. Everything here only plans. It builds an unsigned transaction with kob-wasm
// through the cancel.ts planners. Signing and broadcasting go through <TxFlow> only
// (ui/confirm/send.ts).
import { buildCancelEnv, readClock } from '../../app/env';
import type { Services } from '../../app/services';
import type { StrayView, TokenUtxoView } from '../../data/indexer-types';
import {
  SnapshotError, planCancel, planCancelAll, planCancelReplace, planRefund, planSweep, secondTokenOf, snapshotFromOrderView, snapshotFromRecord, splitStrayViews,
  type CancelPlan, type OrderSnapshot, type ReplacementSpec,
} from '../../kob/cancel';
import { confirmSnapshotOnNode } from '../../kob/node-verify';
import { resolveRecord, retiredOf, type RecordKind, type RecordStore } from '../../kob/records';
import { curveFactsOf, familyOfKind, quoteCovIdOf, tokenCovIdOf } from '../../kob/order-facts';
import { isLiveStatus, type Position } from '../../kob/positions';
import { custodyState, extensionOfState } from '../../kob/token-state';
import type { BuiltTx, Family, Hex, TokenUtxo } from '../../kob/types';
import { t, tIssue } from '../../i18n';
import { PlainError, buildErrorText } from '../../i18n/build-error';
import { buildMergeLink, mergeCarry, mergeLeaves, planMerge, type MergeLink, type MergeOptions, type MergePlan, type MergeToken } from '../../kob/merge';
import type { TokenInfo } from '../../kob/registry';
import { waitForAcceptance } from '../../wallet/sign';
import { buildReplacement, type AmendInput, type AmendKind, type AmendResult } from './amend';
import { mergeSessionPlan, mergeStep } from './merge-model';
import { spkToAddressFor } from './orders-data';
import type { OrderEntry } from './orders-model';
import type { FlowStep } from './TxFlow';


/**
 * Turns an indexer stray into the token UTXO of the order. The state is taken from the view, or
 * rebuilt from owner, amount and extension commitment.
 */
export function strayToTokenUtxo(v: TokenUtxoView, ext: Hex | null, family: Family = v.family ?? 'kcc20'): TokenUtxo | null {
  const state = v.state ?? (family === 'kron' || ext ? custodyState(family, v.amount, v.owner, ext) : null);
  if (!state) return null;
  return { transactionId: v.txid, index: v.index, amount: v.value, blockDaaScore: String(v.created_daa), covenantId: v.token, state };
}

/**
 * The order's current UTXO with custody and strays. The indexer's single-order view and the
 * placement record only say where to look. Every UTXO that ends up in the snapshot (order, custody
 * and strays) is re-read from the node, and its amount, script and covenant id are the node's. The
 * KAS value of unsigned covenant inputs is committed by no signature. When the indexer's answer
 * does not verify on the node, the node-resolved placement record is the primary path. Without a
 * record the mismatch is reported and nothing is built.
 */
export async function snapshotFor(s: Services, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<OrderSnapshot> {
  const toAddress = spkToAddressFor(s);
  const onNode = (snap: OrderSnapshot) => confirmSnapshotOnNode(s.kob, s.node, snap, toAddress);
  let indexerFailure: unknown = null;
  if (s.indexer) {
    try {
      const detail = await s.indexer.order(entry.id);
      if (detail && detail.state_known && detail.current) {
        // An order under a retired template of the cancel-only table. Its view names that template,
        // and its script is that template's.
        const ret = retiredOf(s.kob, { kind: detail.contract as RecordKind, templateHash: detail.template_hash });
        const snap0 = snapshotFromOrderView(detail);
        const snap = ret ? { ...snap0, retired: ret.hash } : snap0;
        // An ask without a known custody UTXO cannot be cancelled from the indexer's data alone.
        // The node lookup below may find it.
        const needsCustody = entry.view?.side === 1 || detail.side === 1;
        if (!needsCustody || snap.custody || !entry.record) return await onNode(snap);
      }
    } catch (e) {
      indexerFailure = e;
    }
  }
  if (entry.record) {
    const resolved = await resolveRecord(s.kob, s.node, entry.record, toAddress);
    if (resolved.status === 'live' && resolved.order) {
      const ext = (resolved.custody ? extensionOfState(resolved.custody.state) : null) ?? (entry.record.custody?.tokenState ? extensionOfState(entry.record.custody.tokenState) : null);
      const family = familyOfKind(entry.record.kind);
      // A pair order's covenant id may also own strays of its quote token B, with its own family
      // and extension commitment.
      const b = secondTokenOf(resolved.order.state);
      const { views, unknown } = await straysOfOrder(s, entry.id, strays);
      // Strays of any other token are foreign. They are kept apart with the program their state was
      // proven under, or reported as unproven.
      const split = splitStrayViews(views, resolved.order.state, curveFactsOf(resolved.order.state)?.unbound ? (resolved.custody?.covenantId ?? null) : null);
      const mapped = split.own.map((x) => (b && x.token === b.covenantId ? strayToTokenUtxo(x, b.family === 'kron' ? null : b.ext, b.family) : strayToTokenUtxo(x, ext, family)));
      const mine = mapped.filter((x): x is TokenUtxo => x !== null);
      const snap0 = snapshotFromRecord(resolved, mine, entry.record.deadline != null ? BigInt(entry.record.deadline) : null);
      const snap: OrderSnapshot = { ...snap0, ...(split.foreign.length ? { foreign: split.foreign } : {}), ...(split.unproven.length ? { foreignUnproven: split.unproven } : {}) };
      // A stray whose state cannot be rebuilt, because the extension commitment is unknown, is not
      // swept either. Say so rather than lose it silently.
      return await onNode(unknown || mine.length < mapped.length ? { ...snap, straysUnknown: true } : snap);
    }
    if (resolved.status === 'old-template') throw new SnapshotError('old-template', `order ${entry.id} was placed with an order template this build does not pin`);
    if (resolved.status === 'unknown') throw new SnapshotError('state-unknown', `order ${entry.id}: its current state could not be found on the node`);
    throw new SnapshotError('not-live', `order ${entry.id} is ${resolved.status}: nothing to spend`);
  }
  if (indexerFailure) throw indexerFailure;
  throw new SnapshotError('state-unknown', `order ${entry.id}: neither the indexer nor a placement record can locate it`);
}

/**
 * Strays of one order for the record path. It reads the indexer's per-owner token UTXOs
 * (`/v1/token-utxos?owner=<covenant id>`, paginated). The maker-wide `/v1/strays` list is capped,
 * at 50 by default, newest first and including lost strays of closed orders, and it can hide the
 * live order's strays. This falls back to that list when the route is not served. `unknown` means
 * neither source could be read, and `strays === null` is a failed list fetch. A cancel may then
 * leave strays behind, so the plan carries a warning (`cancel.strays-unknown`) for the record.
 */
async function straysOfOrder(s: Services, id: Hex, strays: readonly StrayView[] | null): Promise<{ views: TokenUtxoView[]; unknown: boolean }> {
  const fromList = (strays ?? []).filter((x) => x.owner === id && !x.spent);
  if (s.indexer && typeof s.indexer.tokenUtxos === 'function') {
    try {
      const per = await s.indexer.tokenUtxos({ owner: id, limit: 200 });
      if (per) return { views: per.filter((u) => u.role === 'stray' && !u.spent && u.owner === id), unknown: false };
    } catch {
      return { views: fromList, unknown: true };
    }
  }
  return { views: fromList, unknown: strays === null };
}

/**
 * Snapshots of several orders, 4 in parallel. Orders that cannot be located are reported and not
 * silently dropped.
 */
export async function snapshotsFor(s: Services, entries: readonly OrderEntry[], strays: readonly StrayView[] | null = []): Promise<{ snapshots: OrderSnapshot[]; failed: { id: Hex; error: unknown }[] }> {
  const snapshots: OrderSnapshot[] = [];
  const failed: { id: Hex; error: unknown }[] = [];
  const queue = [...entries];
  const worker = async () => {
    for (let e = queue.shift(); e; e = queue.shift()) {
      try {
        snapshots.push(await snapshotFor(s, e, strays));
      } catch (error) {
        failed.push({ id: e.id, error });
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(4, entries.length) }, worker));
  // Keep the caller's order.
  const order = new Map(entries.map((e, i) => [e.id, i]));
  snapshots.sort((a, b) => (order.get(a.covenantId) ?? 0) - (order.get(b.covenantId) ?? 0));
  return { snapshots, failed };
}

const tokenOf = (s: Services, tokenId: Hex | null) => (tokenId ? s.registry.byCovenantId.get(tokenId) : undefined);

export interface PlannedAction {
  plan: CancelPlan;
  snapshot: OrderSnapshot;
}

/**
 * A refusal that only abandoning the smallest strays can get past, because there are more strays
 * than the token program's input room.
 */
const straysExceedSlots = (p: CancelPlan): boolean => !p.ok && p.issues.some((i) => i.code === 'cancel.strays-exceed-slots');

/**
 * A cancel plan that abandons strays because one transaction cannot move them all. A sweep first
 * moves them while the order lives.
 */
export const abandonsStrays = (p: CancelPlan): boolean => p.ok && p.issues.some((i) => i.code === 'cancel.strays-abandoned');

/**
 * Plans the cancel of one order. Anyone can send dust token UTXOs to an order's covenant id. With
 * more of them than one transaction can move, sweeping them all in the cancel is impossible.
 * Instead of leaving the order uncancellable, the plan is rebuilt abandoning the smallest strays,
 * and it carries a `cancel.strays-abandoned` warning with count and amount. The UI offers "Sweep
 * first" for such a plan (`abandonsStrays`). A sweep in place moves up to the program's inputs per
 * transaction while the order lives on, and the cancel then takes the rest.
 */
export async function planCancelFor(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<PlannedAction> {
  const snapshot = await snapshotFor(s, entry, strays);
  const env = await buildCancelEnv(s, { pubkey });
  const plan = planCancel(env, snapshot);
  return { plan: straysExceedSlots(plan) ? planCancel(env, snapshot, { allowAbandonStrays: true }) : plan, snapshot };
}

/**
 * Plans the maker's sweep of one live order's strays in place (kob/cancel.ts `planSweep`). The
 * order continues unchanged, and its strays return to the wallet. These are its own token, a pair
 * order's B and foreign ones with a proven state. A non-ask order needs one wallet KAS UTXO for the
 * fee.
 */
export async function planSweepFor(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<PlannedAction> {
  const snapshot = await snapshotFor(s, entry, strays);
  const env = await buildCancelEnv(s, { pubkey });
  return { plan: planSweep(env, snapshot), snapshot };
}

/**
 * Sweeps of several orders ("Sweep first" before a cancel that would abandon strays). It builds one
 * transaction per order, submitted one after the other, so a wallet funding UTXO one sweep spends
 * is not offered to the next.
 */
export async function planSweepMany(s: Services, pubkey: Hex, entries: readonly OrderEntry[], strays: readonly StrayView[] | null = []): Promise<PlannedMany> {
  const { snapshots, failed } = await snapshotsFor(s, entries, strays);
  if (!snapshots.length) return { plans: [], snapshots, failed };
  const env = await buildCancelEnv(s, { pubkey });
  const spent = new Set<string>();
  const plans = snapshots.map((snap) => {
    const plan = planSweep({ ...env, funding: (env.funding ?? []).filter((f) => !spent.has(`${f.transactionId}:${f.index}`)) }, snap);
    for (const f of plan.fundingUsed) spent.add(`${f.transactionId}:${f.index}`);
    return plan;
  });
  return { plans, snapshots, failed };
}

/** The orders a cancel plan abandons strays of, as their ids from the plan's own findings. */
export function abandoningOrders(plans: readonly CancelPlan[]): Hex[] {
  const ids = plans.flatMap((p) => (p.ok ? p.issues.filter((i) => i.code === 'cancel.strays-abandoned').map((i) => String(i.params?.order ?? '')) : []));
  return [...new Set(ids.filter((x) => /^[0-9a-f]{64}$/.test(x)))];
}

export async function planRefundFor(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<PlannedAction> {
  const snapshot = await snapshotFor(s, entry, strays);
  const env = await buildCancelEnv(s, { pubkey });
  // Reclaiming the tip needs one wallet signature and returns most of the tip to the maker.
  // `planRefund` falls back to no signature without funding.
  return { plan: planRefund(env, snapshot, { reclaimTip: true }), snapshot };
}

/**
 * Live booked exits of a repeat entry. Cancelling the entry alone leaves them without their
 * take-profit until their rptUntil, up to 90 days, while their stop-loss and cancel keep working.
 * The position cancel is therefore the right action. Returns their count, 0 when it is not a repeat
 * entry or none is live.
 */
export function liveExitsOfRepeatEntry(entry: OrderEntry, positions: readonly Position[]): number {
  if (entry.view?.repeat?.role !== 'entry') return 0;
  const pos = positions.find((p) => p.entry?.covenant_id === entry.id);
  return pos ? pos.exits.filter((x) => isLiveStatus(x.status)).length : 0;
}

/**
 * Adds the "entry-only cancel strands the exits' take-profit" warning to a plan that cancels only
 * the entry.
 */
export function warnEntryOnly(plan: CancelPlan, liveExits: number): CancelPlan {
  if (!plan.ok || liveExits <= 0) return plan;
  return {
    ...plan,
    issues: [...plan.issues, { code: 'cancel.repeat-entry-only', severity: 'warning', message: `cancelling only the repeat entry: its ${liveExits} live exit(s) keep their stop-loss but cannot take profit until their repeat window ends (up to 90 days); cancel the position instead`, params: { exits: liveExits } }],
  };
}

export interface PlannedMany {
  plans: CancelPlan[];
  snapshots: OrderSnapshot[];
  failed: { id: Hex; error: unknown }[];
}

/**
 * Cancels several orders with the fewest transactions, grouped per token and split by the token
 * program's input slots.
 */
export async function planCancelMany(s: Services, pubkey: Hex, entries: readonly OrderEntry[], strays: readonly StrayView[] | null = []): Promise<PlannedMany> {
  const { snapshots, failed } = await snapshotsFor(s, entries, strays);
  if (!snapshots.length) return { plans: [], snapshots, failed };
  const env = await buildCancelEnv(s, { pubkey });
  // Griefing dust strays must not make a position uncancellable. An order with more strays than its
  // transaction can move is re-planned abandoning its smallest strays. That carries a
  // `cancel.strays-abandoned` warning, and My orders offers "Sweep first" for it.
  const plans = planCancelAll(env, snapshots);
  return { plans: plans.some(straysExceedSlots) ? planCancelAll(env, snapshots, { allowAbandonStrays: true }) : plans, snapshots, failed };
}

export interface PlannedAmend {
  amend: AmendResult;
  plan: CancelPlan | null;
  snapshot: OrderSnapshot;
}

/**
 * Amendment of one order. It builds the replacement spec from the current state, then the atomic
 * cancel + replace plan. The kind comes from `amendKind` (orders-model).
 */
export async function planAmendFor(s: Services, pubkey: Hex, entry: OrderEntry, kind: AmendKind, input: AmendInput, strays: readonly StrayView[] | null = []): Promise<PlannedAmend> {
  const snapshot = await snapshotFor(s, entry, strays);
  const token = tokenOf(s, tokenCovIdOf(snapshot.order.state));
  // A renewal needs the current DAA, because the new expiry is now + 90 days.
  const nowDaa = input.renew ? (await readClock(s)).daa : undefined;
  const amend = buildReplacement(snapshot, kind, input, { tick: token?.tick ?? undefined, kob: s.kob, ...(nowDaa !== undefined ? { nowDaa } : {}) });
  if (!amend.ok || !amend.spec) return { amend, plan: null, snapshot };
  const env = await buildCancelEnv(s, { pubkey }, token);
  return { amend, plan: planCancelReplace(env, snapshot, amend.spec), snapshot };
}

/**
 * Replaces an order by cancel-replace from the full ticket form. `spec` is the order the ticket
 * planned, with its state, KAS value, token carrier and day-order deadline. The current order is
 * snapshotted from the node, and both go into one atomic transaction (`planCancelReplace`). An
 * armed stop restarts unarmed, custody and strays are swept, and a larger amount is topped up from
 * free tokens.
 */
export async function planReplaceFor(s: Services, pubkey: Hex, entry: OrderEntry, spec: ReplacementSpec, strays: readonly StrayView[] | null = []): Promise<{ plan: CancelPlan; snapshot: OrderSnapshot }> {
  const snapshot = await snapshotFor(s, entry, strays);
  const token = tokenOf(s, tokenCovIdOf(snapshot.order.state));
  const env0 = await buildCancelEnv(s, { pubkey }, token);
  // For a pair order a larger replacement may need more of the quote token B too. Its free UTXOs
  // join the base token's.
  const quote = tokenOf(s, quoteCovIdOf(snapshot.order.state));
  const quoteUtxos = quote && quote.program ? await s.tracker.tokenUtxosFor(pubkey, { covenantId: quote.covenantId, program: quote.program }) : [];
  const env = quoteUtxos.length ? { ...env0, tokenUtxos: [...(env0.tokenUtxos ?? []), ...quoteUtxos] } : env0;
  return { plan: planCancelReplace(env, snapshot, spec), snapshot };
}

/**
 * After a submitted cancel, refund or replace, the records are marked and not dropped. The mark
 * `cancelling` holds the tx id, the order outpoint it spends and the time. A cancel may lose to a
 * fill or be reorged out, and with the indexer gone the record is the only way to find the order
 * again. `loadOrders` drops the record once the order is seen terminal or spent, and clears the
 * mark when the order lives on (orders-data.ts `syncRecords`).
 */
export async function markCancelling(store: RecordStore, plan: Pick<CancelPlan, 'cancelIds' | 'built'>, txid: Hex, nowUnix: bigint = BigInt(Math.floor(Date.now() / 1000))): Promise<void> {
  for (const id of plan.cancelIds) {
    // An in-place amend continues the covenant id in this tx, with the same id and a new state. The
    // order is not being cancelled, and its record moved (records.ts `amendedRecords`).
    if (plan.built?.tx.outputs.some((o) => o.covenant?.covenantId === id)) continue;
    try {
      const rec = await store.get(id);
      if (!rec) continue;
      const input = plan.built?.tx.inputs.find((i) => i.utxo?.covenantId === id);
      await store.put({ ...rec, cancelling: { txid, spends: input ? `${input.transactionId}:${input.index}` : '', atUnix: nowUnix.toString() } });
    } catch {
      /*
       * The record store is a convenience. A failure must not turn an accepted transaction into an
       * error.
       */
    }
  }
}

/**
 * Drops the records of orders that are gone for good, such as in tests and for records the user
 * removes.
 */
export async function dropRecords(store: RecordStore, ids: readonly Hex[]): Promise<void> {
  for (const id of ids) {
    try {
      await store.remove(id);
    } catch {
      /*
       * The record store is a convenience. A failure must not turn an accepted transaction into an
       * error.
       */
    }
  }
}

// Token merge.

/**
 * Plans the merge of the wallet's plain UTXOs of `token` as a tree (kob/merge.ts). With no `target`
 * it is "Merge all". It merges every mergeable UTXO into one, in at most MAX_MERGE_TXS
 * transactions. With a `target` it is the sell ticket's merge. It merges only until one placement
 * can take the amount. The token UTXOs come from the tracker (indexer and local candidates,
 * verified on the node) without what this page has just spent, and never one of `reserved` (the
 * open orders' custodies and strays). A merge is manual and costs a fee, so nothing merges until
 * the user confirms the plan.
 */
export async function planMergeFor(s: Services, pubkey: Hex, token: MergeToken | null, reserved: ReadonlySet<string>, o: MergeOptions = {}): Promise<MergePlan> {
  if (!token) throw new PlainError(t('orders.merge.unsupported'));
  const env = await buildCancelEnv(s, { pubkey }, token);
  return planMerge(env, token, env.tokenUtxos ?? [], reserved, o);
}

/** How long a merge waits for the outputs it spends to be in the node's UTXO set. */
export const MERGE_PARENT_TIMEOUT_MS = 60_000;

/**
 * The outputs of the sent links `ids` on the node, in that order. It is null when one is not there
 * in time.
 */
async function sentOutputs(s: Services, ids: readonly number[], sent: readonly MergeLink[], txids: readonly string[], timeoutMs: number) {
  const toAddress = spkToAddressFor(s);
  const want = ids.map((k) => {
    const l = sent[k]!;
    return { address: toAddress(l.built.tx.outputs[l.output.index]!.scriptPublicKey), transactionId: txids[k]!, index: l.output.index };
  });
  const res = await waitForAcceptance(s.node, want, { timeoutMs });
  if (!res.accepted) return null;
  return want.map((w) => res.found.find((f) => f.transactionId === w.transactionId && f.index === w.index)!);
}

/**
 * Link `k` of a running merge, built again at its turn. `sent` holds the links sent so far and
 * `txids` their transaction ids. Its parents' outputs are read from the node, once the node lists
 * them. KOB's node lists an output once its transaction is in a block, and the send re-reads every
 * token input there before the wallet is asked. The KAS UTXOs are read afresh, without the ones the run
 * has spent. A link of level 1 waits for nothing. The wallet's own node is the send's visibility
 * gate (wallet/visibility.ts).
 */
export async function nextMergeLink(
  s: Services, pubkey: Hex, token: MergeToken, plan: MergePlan, k: number, sent: readonly MergeLink[], txids: readonly string[], timeoutMs = MERGE_PARENT_TIMEOUT_MS,
): Promise<MergeLink> {
  const planned = plan.links[k];
  if (!planned) throw new PlainError(t('orders.merge.unsupported'));
  const found = planned.parents.length ? await sentOutputs(s, planned.parents, sent, txids, timeoutMs) : [];
  if (!found) throw new PlainError(t('orders.merge.notAccepted'));
  const carried = planned.parents.map((p, i) => mergeCarry(sent[p]!, txids[p]!, token, found[i]));
  const env = await buildCancelEnv(s, { pubkey });
  const link = buildMergeLink(env, token, [...carried, ...planned.fresh], planned.fresh, new Set(), planned);
  if ('error' in link) throw new Error(tIssue('orders.issue', link.error));
  return link;
}

/**
 * A planned merge as the steps of a <TxFlow>. Each later step is built at its turn
 * (`nextMergeLink`).
 */
export interface MergeFlow {
  steps: FlowStep[];
  next(index: number, txids: readonly string[]): Promise<FlowStep>;
  /** The links as built for the send so far, by step. */
  sent: MergeLink[];
  /**
   * The plan a wallet may approve once (merge-model.ts `mergeSessionPlan`). There is none for KRON.
   */
  session?: BuiltTx[];
}

/**
 * Runs `plan`, planned by `planMergeFor`, as a flow whose every step shows `summary`, one line from
 * merge-model.ts `mergeRunLine`.
 */
export function mergeFlow(s: Services, pubkey: Hex, token: MergeToken, plan: MergePlan, summary: string): MergeFlow {
  const total = plan.links.length;
  const sent: MergeLink[] = plan.links.slice(0, 1);
  return {
    steps: plan.links.map((l, i) => mergeStep(l, i, total, summary, plan.convert)),
    sent,
    ...(mergeSessionPlan(plan, token) ? { session: mergeSessionPlan(plan, token)! } : {}),
    next: async (k, txids) => {
      const link = await nextMergeLink(s, pubkey, token, plan, k, sent, txids);
      sent[k] = link;
      return mergeStep(link, k, total, summary, plan.convert);
    },
  };
}

/**
 * Waits until the node lists the outputs a merge run left, which are the leaves of the links it
 * sent. The sale after the sell ticket's merge plans from them.
 */
export async function awaitMergeOutputs(s: Services, sent: readonly MergeLink[], txids: readonly string[], timeoutMs = MERGE_PARENT_TIMEOUT_MS): Promise<boolean> {
  const leaves = mergeLeaves(sent).filter((k) => txids[k]);
  return leaves.length === 0 || (await sentOutputs(s, leaves, sent, txids, timeoutMs)) !== null;
}

/**
 * The user-facing text of a planning failure. A raw builder or node text becomes a plain sentence,
 * and `rawActionError` keeps it for "Details".
 */
export function describeActionError(e: unknown): string {
  if (e instanceof SnapshotError) return t(`orders.error.snapshot.${e.code}`);
  if (e instanceof PlainError) return e.message;
  return buildErrorText(e instanceof Error ? e.message : String(e));
}

/**
 * The raw text of a planning failure for "Details". It is null for the known snapshot errors, whose
 * sentence says it all.
 */
export function rawActionError(e: unknown): string | null {
  if (e instanceof SnapshotError || e instanceof PlainError) return null;
  return e instanceof Error ? e.message : String(e);
}
