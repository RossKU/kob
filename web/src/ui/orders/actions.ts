// Planning of the order actions: cancel, cancel-all (position / token / everything), cancel-replace (amend) and refund. Everything here only PLANS
// (builds an unsigned transaction with kob-wasm through the cancel.ts planners); signing and broadcasting go through <ConfirmSign> only.
import { buildCancelEnv, readClock } from '../../app/env';
import type { Services } from '../../app/services';
import type { StrayView, TokenUtxoView } from '../../data/indexer-types';
import {
  SnapshotError, planCancel, planCancelAll, planCancelReplace, planRefund, planSweep, secondTokenOf, snapshotFromOrderView, snapshotFromRecord, splitStrayViews,
  type CancelPlan, type OrderSnapshot, type ReplacementSpec,
} from '../../kob/cancel';
import { confirmSnapshotOnNode } from '../../kob/node-verify';
import { resolveRecord, type RecordStore } from '../../kob/records';
import { familyOfKind, quoteCovIdOf, tokenCovIdOf } from '../../kob/order-facts';
import { isLiveStatus, type Position } from '../../kob/positions';
import { custodyState, extensionOfState } from '../../kob/token-state';
import type { Family, Hex, TokenUtxo } from '../../kob/types';
import { t } from '../../i18n';
import { buildErrorText } from '../../i18n/build-error';
import { buildReplacement, type AmendInput, type AmendKind, type AmendResult } from './amend';
import { spkToAddressFor } from './orders-data';
import type { OrderEntry } from './orders-model';


/** Indexer stray -> token UTXO of the order (the state is taken from the view, or rebuilt from owner + amount + extension commitment). */
export function strayToTokenUtxo(v: TokenUtxoView, ext: Hex | null, family: Family = v.family ?? 'kcc20'): TokenUtxo | null {
  const state = v.state ?? (family === 'kron' || ext ? custodyState(family, v.amount, v.owner, ext) : null);
  if (!state) return null;
  return { transactionId: v.txid, index: v.index, amount: v.value, blockDaaScore: String(v.created_daa), covenantId: v.token, state };
}

/**
 * The order's current UTXO with custody and strays. The indexer's single-order view and the placement record only say WHERE to look: every
 * UTXO that ends up in the snapshot (order, custody, strays) is re-read from the NODE and its amount, script and covenant id are the node's
 * (the KAS value of unsigned covenant inputs is committed by no signature). When the indexer's answer does not verify on the node the
 * node-resolved placement record is the primary path; without a record the mismatch is reported and nothing is built.
 */
export async function snapshotFor(s: Services, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<OrderSnapshot> {
  const toAddress = spkToAddressFor(s);
  const onNode = (snap: OrderSnapshot) => confirmSnapshotOnNode(s.kob, s.node, snap, toAddress);
  let indexerFailure: unknown = null;
  if (s.indexer) {
    try {
      const detail = await s.indexer.order(entry.id);
      if (detail && detail.state_known && detail.current) {
        const snap = snapshotFromOrderView(detail);
        // an ask without a known custody UTXO cannot be cancelled from the indexer's data alone: the node lookup below may find it
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
      // a pair order's covenant id may also own strays of its quote token B (its own family and extension commitment)
      const b = secondTokenOf(resolved.order.state);
      const { views, unknown } = await straysOfOrder(s, entry.id, strays);
      // strays of any other token are foreign: kept apart with the program their state was proven under (or reported as unproven)
      const split = splitStrayViews(views, resolved.order.state);
      const mapped = split.own.map((x) => (b && x.token === b.covenantId ? strayToTokenUtxo(x, b.family === 'kron' ? null : b.ext, b.family) : strayToTokenUtxo(x, ext, family)));
      const mine = mapped.filter((x): x is TokenUtxo => x !== null);
      const snap0 = snapshotFromRecord(resolved, mine, entry.record.deadline != null ? BigInt(entry.record.deadline) : null);
      const snap: OrderSnapshot = { ...snap0, ...(split.foreign.length ? { foreign: split.foreign } : {}), ...(split.unproven.length ? { foreignUnproven: split.unproven } : {}) };
      // a stray whose state cannot be rebuilt (extension commitment unknown) is not swept either: say so rather than lose it silently
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
 * Strays of ONE order for the record path: the indexer's per-owner token UTXOs (`/v1/token-utxos?owner=<covenant id>`, paginated) rather than the
 * maker-wide `/v1/strays` list, which is capped (50 by default, newest first, lost strays of closed orders included) and can hide the live order's
 * strays. Falls back to that list when the route is not served. `unknown`: neither source could be read (`strays === null` is a failed list
 * fetch), so a cancel may leave strays behind: the plan then carries a warning the user acknowledges, never a silent loss.
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

/** Snapshots of several orders (4 in parallel); orders that cannot be located are reported, not silently dropped. */
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
  // keep the caller's order
  const order = new Map(entries.map((e, i) => [e.id, i]));
  snapshots.sort((a, b) => (order.get(a.covenantId) ?? 0) - (order.get(b.covenantId) ?? 0));
  return { snapshots, failed };
}

const tokenOf = (s: Services, tokenId: Hex | null) => (tokenId ? s.registry.byCovenantId.get(tokenId) : undefined);

export interface PlannedAction {
  plan: CancelPlan;
  snapshot: OrderSnapshot;
}

/** A refusal that only abandoning (the smallest) strays can get past: more strays than the token program's input room (C5-01). */
const straysExceedSlots = (p: CancelPlan): boolean => !p.ok && p.issues.some((i) => i.code === 'cancel.strays-exceed-slots');

/** A cancel plan that abandons strays because one transaction cannot move them all (C5-01): a sweep first moves them while the order lives. */
export const abandonsStrays = (p: CancelPlan): boolean => p.ok && p.issues.some((i) => i.code === 'cancel.strays-abandoned');

/**
 * Plans the cancel of one order. Anyone can send dust token UTXOs to an order's covenant id; with more of them than one transaction can move,
 * sweeping them all in the cancel is impossible, so instead of leaving the order uncancellable the plan is rebuilt abandoning the SMALLEST strays:
 * it then carries a `cancel.strays-abandoned` warning (count, amount) that the confirm screen makes the user acknowledge before signing. The UI
 * offers "Sweep first" for such a plan (`abandonsStrays`): a sweep in place moves up to the program's inputs per transaction while the order
 * lives on, and the cancel then takes the rest.
 */
export async function planCancelFor(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<PlannedAction> {
  const snapshot = await snapshotFor(s, entry, strays);
  const env = await buildCancelEnv(s, { pubkey });
  const plan = planCancel(env, snapshot);
  return { plan: straysExceedSlots(plan) ? planCancel(env, snapshot, { allowAbandonStrays: true }) : plan, snapshot };
}

/**
 * Plans the maker's sweep of one live order's strays IN PLACE (kob/cancel.ts `planSweep`): the order continues unchanged, its strays (own token,
 * a pair order's B, foreign ones with a proven state) return to the wallet; a non-ask order needs one wallet KAS UTXO for the fee.
 */
export async function planSweepFor(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<PlannedAction> {
  const snapshot = await snapshotFor(s, entry, strays);
  const env = await buildCancelEnv(s, { pubkey });
  return { plan: planSweep(env, snapshot), snapshot };
}

/**
 * Sweeps of several orders ("Sweep first" before a cancel that would abandon strays): one transaction per order, submitted one after the other, so a
 * wallet funding UTXO one sweep spends is not offered to the next.
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

/** The orders a cancel plan abandons strays of (C5-01): their ids, from the plan's own findings. */
export function abandoningOrders(plans: readonly CancelPlan[]): Hex[] {
  const ids = plans.flatMap((p) => (p.ok ? p.issues.filter((i) => i.code === 'cancel.strays-abandoned').map((i) => String(i.params?.order ?? '')) : []));
  return [...new Set(ids.filter((x) => /^[0-9a-f]{64}$/.test(x)))];
}

export async function planRefundFor(s: Services, pubkey: Hex, entry: OrderEntry, strays: readonly StrayView[] | null = []): Promise<PlannedAction> {
  const snapshot = await snapshotFor(s, entry, strays);
  const env = await buildCancelEnv(s, { pubkey });
  // reclaiming the tip needs one wallet signature and returns most of the tip to the maker (planRefund falls back to no signature without funding)
  return { plan: planRefund(env, snapshot, { reclaimTip: true }), snapshot };
}

/**
 * Live booked exits of a repeat entry: cancelling the entry ALONE leaves them without their take-profit until their rptUntil (up to 90 days;
 * their stop-loss and cancel keep working), so the position cancel is the right action. Returns their count (0: not a repeat entry / none live).
 */
export function liveExitsOfRepeatEntry(entry: OrderEntry, positions: readonly Position[]): number {
  if (entry.view?.repeat?.role !== 'entry') return 0;
  const pos = positions.find((p) => p.entry?.covenant_id === entry.id);
  return pos ? pos.exits.filter((x) => isLiveStatus(x.status)).length : 0;
}

/** Adds the "entry-only cancel strands the exits' take-profit" warning (acknowledged on the confirm screen) to a plan that cancels only the entry. */
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

/** Cancel several orders with the fewest transactions (grouped per token, split by the token program's input slots). */
export async function planCancelMany(s: Services, pubkey: Hex, entries: readonly OrderEntry[], strays: readonly StrayView[] | null = []): Promise<PlannedMany> {
  const { snapshots, failed } = await snapshotsFor(s, entries, strays);
  if (!snapshots.length) return { plans: [], snapshots, failed };
  const env = await buildCancelEnv(s, { pubkey });
  // griefing dust strays must not make a position uncancellable: an order with more strays than its transaction can move is re-planned
  // abandoning its smallest strays (a `cancel.strays-abandoned` warning the confirm screen makes the user acknowledge)
  const plans = planCancelAll(env, snapshots);
  return { plans: plans.some(straysExceedSlots) ? planCancelAll(env, snapshots, { allowAbandonStrays: true }) : plans, snapshots, failed };
}

export interface PlannedAmend {
  amend: AmendResult;
  plan: CancelPlan | null;
  snapshot: OrderSnapshot;
}

/** Amendment of one order: the replacement spec from the current state, then the atomic cancel + replace plan. The kind comes from amendKind (orders-model). */
export async function planAmendFor(s: Services, pubkey: Hex, entry: OrderEntry, kind: AmendKind, input: AmendInput, strays: readonly StrayView[] | null = []): Promise<PlannedAmend> {
  const snapshot = await snapshotFor(s, entry, strays);
  const token = tokenOf(s, tokenCovIdOf(snapshot.order.state));
  // a renewal needs the current DAA (new expiry = now + 90 days)
  const nowDaa = input.renew ? (await readClock(s)).daa : undefined;
  const amend = buildReplacement(snapshot, kind, input, { tick: token?.tick ?? undefined, kob: s.kob, ...(nowDaa !== undefined ? { nowDaa } : {}) });
  if (!amend.ok || !amend.spec) return { amend, plan: null, snapshot };
  const env = await buildCancelEnv(s, { pubkey }, token);
  return { amend, plan: planCancelReplace(env, snapshot, amend.spec), snapshot };
}

/**
 * Replace by cancel-replace from the full ticket form: `spec` is the order the ticket planned (its state, KAS value, token carrier and day-order
 * deadline); the current order is snapshotted from the node and both go into ONE atomic transaction (planCancelReplace: an armed stop restarts
 * unarmed, custody and strays are swept, a larger amount is topped up from free tokens).
 */
export async function planReplaceFor(s: Services, pubkey: Hex, entry: OrderEntry, spec: ReplacementSpec, strays: readonly StrayView[] | null = []): Promise<{ plan: CancelPlan; snapshot: OrderSnapshot }> {
  const snapshot = await snapshotFor(s, entry, strays);
  const token = tokenOf(s, tokenCovIdOf(snapshot.order.state));
  const env0 = await buildCancelEnv(s, { pubkey }, token);
  // a pair order: a larger replacement may need more of the quote token B too (its free UTXOs join the base token's)
  const quote = tokenOf(s, quoteCovIdOf(snapshot.order.state));
  const quoteUtxos = quote && quote.program ? await s.tracker.tokenUtxosFor(pubkey, { covenantId: quote.covenantId, program: quote.program }) : [];
  const env = quoteUtxos.length ? { ...env0, tokenUtxos: [...(env0.tokenUtxos ?? []), ...quoteUtxos] } : env0;
  return { plan: planCancelReplace(env, snapshot, spec), snapshot };
}

/**
 * After a submitted cancel / refund / replace: the records are MARKED (`cancelling`: tx id, the order outpoint it spends, when), not dropped. A
 * cancel may lose to a fill or be reorged out, and with the indexer gone the record is the only way to find the order again; loadOrders drops
 * the record once the order is seen terminal / spent, and clears the mark when the order lives on (orders-data.ts `syncRecords`).
 */
export async function markCancelling(store: RecordStore, plan: Pick<CancelPlan, 'cancelIds' | 'built'>, txid: Hex, nowUnix: bigint = BigInt(Math.floor(Date.now() / 1000))): Promise<void> {
  for (const id of plan.cancelIds) {
    // an in-place amend continues the covenant id in this tx (same id, new state): the order is not being cancelled, its record moved
    // (records.ts `amendedRecords`)
    if (plan.built?.tx.outputs.some((o) => o.covenant?.covenantId === id)) continue;
    try {
      const rec = await store.get(id);
      if (!rec) continue;
      const input = plan.built?.tx.inputs.find((i) => i.utxo?.covenantId === id);
      await store.put({ ...rec, cancelling: { txid, spends: input ? `${input.transactionId}:${input.index}` : '', atUnix: nowUnix.toString() } });
    } catch {
      /* the record store is a convenience: a failure must not turn an accepted transaction into an error */
    }
  }
}

/** Drops the records of orders that are gone for good (tests, and records the user removes). */
export async function dropRecords(store: RecordStore, ids: readonly Hex[]): Promise<void> {
  for (const id of ids) {
    try {
      await store.remove(id);
    } catch {
      /* the record store is a convenience: a failure must not turn an accepted transaction into an error */
    }
  }
}

/** The user-facing text of a planning failure (a raw builder / node text becomes a plain sentence; `rawActionError` keeps it for "Details"). */
export function describeActionError(e: unknown): string {
  if (e instanceof SnapshotError) return t(`orders.error.snapshot.${e.code}`);
  return buildErrorText(e instanceof Error ? e.message : String(e));
}

/** The raw text of a planning failure for "Details" (null for the known snapshot errors, whose sentence says it all). */
export function rawActionError(e: unknown): string | null {
  if (e instanceof SnapshotError) return null;
  return e instanceof Error ? e.message : String(e);
}
