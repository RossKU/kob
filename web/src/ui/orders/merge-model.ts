// "Merge tokens" in My orders > Balances: which token UTXOs a merge may take, how many per token, the flow step of a planned merge and the summary
// its confirmation shows. One click = ONE transaction (planMergeFor plans one link: the smallest up-to-3 UTXOs into one): a merge is manual and
// every transaction costs a fee, so the user decides each one and clicks again to merge more. Pure; the builder is kob/merge.ts.
import type { OrderView, StrayView } from '../../data/indexer-types';
import { t } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import { mergeCandidates, mergeSlots, outpointOf, type MergeLink, type MergePlan, type MergeToken } from '../../kob/merge';
import { DEFAULT_CARRIER } from '../../kob/orders/common';
import { formatKas } from '../../kob/order-facts';
import type { TokenInfo } from '../../kob/registry';
import type { Hex, TokenUtxo } from '../../kob/types';
import type { FlowStep } from './TxFlow';

const LIVE = new Set(['open', 'partial']);

/**
 * Outpoints held for the wallet's open orders: their custodies (a pair order's both) and strays, from the indexer views, the record snapshots
 * and the maker-wide stray list. They are owned by the orders' covenant ids, so the key-owned filter already leaves them out; the merge
 * refuses them by outpoint as well.
 */
export function reservedOutpoints(d: { views: readonly OrderView[]; snapshots: readonly OrderSnapshot[]; strays: readonly StrayView[] } | null): Set<string> {
  const out = new Set<string>();
  if (!d) return out;
  const add = (u: { txid: string; index: number } | null | undefined) => {
    if (u) out.add(`${u.txid}:${u.index}`);
  };
  for (const v of d.views) {
    if (!LIVE.has(v.status)) continue;
    add(v.custody?.utxo);
    for (const c of v.pair?.custodies ?? []) add(c.utxo);
    for (const s of v.strays ?? []) add(s);
  }
  for (const s of d.snapshots) for (const u of [s.custody, s.prefund, ...s.strays]) if (u) out.add(outpointOf(u));
  for (const s of d.strays) if (!s.spent) add(s);
  return out;
}

/** The registry token as the merge builder takes it (null when this build has no program for it). */
export function mergeTokenOf(token: TokenInfo): MergeToken | null {
  if (!token.program) return null;
  return { covenantId: token.covenantId, program: token.program, templateHash: token.templateHash, extensionCommitment: token.extensionCommitment, slots: token.slots };
}

/** Per token: how many of the wallet's free UTXOs a merge may take (tokens with fewer than two are left out). */
export function mergeableCounts(freeTokens: readonly TokenUtxo[], tokens: ReadonlyMap<Hex, TokenInfo>, maker: Hex, reserved: ReadonlySet<string>): Map<Hex, number> {
  const out = new Map<Hex, number>();
  for (const id of new Set(freeTokens.map((u) => u.covenantId).filter((x): x is Hex => !!x))) {
    const info = tokens.get(id);
    const mt = info ? mergeTokenOf(info) : null;
    if (!mt) continue;
    const n = mergeCandidates(freeTokens, mt, maker, reserved).length;
    if (n >= 2) out.set(id, n);
  }
  return out;
}

/**
 * Per token: about how much KAS the next Merge click frees (before its fee, a few hundredths of a KAS): the KAS on the UTXOs one transaction takes
 * (the smallest ones, `mergeCandidates` order, up to the program's inputs) less the carrier the merged UTXO keeps (the largest input's, at most the
 * default carrier: kob/merge.ts `mergedCarrier`). Tokens with fewer than two mergeable UTXOs are left out. The confirmation states the exact amount.
 */
export function nextMergeFrees(freeTokens: readonly TokenUtxo[], tokens: ReadonlyMap<Hex, TokenInfo>, maker: Hex, reserved: ReadonlySet<string>): Map<Hex, bigint> {
  const out = new Map<Hex, bigint>();
  for (const id of new Set(freeTokens.map((u) => u.covenantId).filter((x): x is Hex => !!x))) {
    const info = tokens.get(id);
    const mt = info ? mergeTokenOf(info) : null;
    if (!mt) continue;
    const take = mergeCandidates(freeTokens, mt, maker, reserved).slice(0, mergeSlots(mt));
    if (take.length < 2) continue;
    const kas = take.map((u) => BigInt(u.amount));
    const largest = kas.reduce((m, v) => (v > m ? v : m), 0n);
    const kept = largest < DEFAULT_CARRIER ? largest : DEFAULT_CARRIER;
    out.set(id, kas.reduce((s, v) => s + v, 0n) - kept);
  }
  return out;
}

/**
 * The merge in a few sentences (shown on its confirmation): how many UTXOs it merges and the count before / after, the KAS it frees (the merged
 * UTXOs' carriers, less the one the merged output keeps, less the fee: the reason to merge at all; when the fee exceeds the freed carriers, the
 * net cost instead) and how many mergeable UTXOs are left for the next click. Written for the app's one-link plan; a chain says its count.
 */
export function mergeSummary(plan: Pick<MergePlan, 'before' | 'after' | 'merged' | 'freed' | 'links' | 'fee'>, ticker: string): string {
  const head =
    plan.links.length > 1
      ? t('orders.merge.summaryChain', { ticker, merged: plan.merged, before: plan.before, after: plan.after, txs: plan.links.length })
      : t('orders.merge.summary', { ticker, merged: plan.merged, before: plan.before, after: plan.after });
  const kas =
    plan.freed > 0n
      ? t('orders.merge.freed', { freed: formatKas(plan.freed), fee: formatKas(plan.fee) })
      : t('orders.merge.costs', { cost: formatKas(-plan.freed), fee: formatKas(plan.fee) });
  const rest = plan.after >= 2 ? t('orders.merge.more', { after: plan.after }) : t('orders.merge.last');
  return `${head} ${kas} ${rest}`;
}

/** A link as a flow plan (TxFlow takes the built transaction and the claims to check). */
export function mergeFlowPlan(link: MergeLink): CancelPlan {
  return { ok: true, issues: link.issues, request: null, built: link.built, cancelIds: [], expected: { cancelIds: [] }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: link.fundingUsed };
}

/** The merge's one transaction as a flow step, its confirmation showing `summary`. */
export function mergeStep(link: MergeLink, summary: string): FlowStep {
  return {
    title: t('orders.merge.title'),
    plan: mergeFlowPlan(link),
    spends: [],
    shownAs: summary,
    toast: t('orders.merge.submitted', { count: link.inputs.length }),
  };
}

/** A plan that cannot run, as a flow plan carrying its errors (PlanErrors shows them). */
export function mergeFailure(plan: Pick<MergePlan, 'issues'>): CancelPlan {
  return { ok: false, issues: plan.issues, request: null, built: null, cancelIds: [], expected: { cancelIds: [] }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: [] };
}
