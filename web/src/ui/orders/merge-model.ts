// "Merge tokens" in My orders > Balances: which token UTXOs a merge may take, how many per token, the flow steps of a planned merge and the summary
// the confirmation screens show. Pure; the chain itself is kob/merge.ts.
import type { OrderView, StrayView } from '../../data/indexer-types';
import { t } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import { mergeCandidates, outpointOf, type MergeLink, type MergePlan, type MergeToken } from '../../kob/merge';
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

/** The whole merge in one line (shown on every confirmation screen of the chain). */
export const mergeSummary = (plan: Pick<MergePlan, 'before' | 'after' | 'links' | 'fee'>, ticker: string): string =>
  t('orders.merge.summary', { ticker, before: plan.before, after: plan.after, txs: plan.links.length, fee: formatKas(plan.fee) });

/** A link as a flow plan (TxFlow / ConfirmSign take the built transaction, the claims to check and the plan's notes). */
export function mergeFlowPlan(link: MergeLink): CancelPlan {
  return { ok: true, issues: link.issues, request: null, built: link.built, cancelIds: [], expected: { cancelIds: [] }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: link.fundingUsed };
}

/** Link `n` (0-based) of `total` as a flow step. */
export function mergeStep(link: MergeLink, n: number, total: number, summary: string): FlowStep {
  return {
    title: total > 1 ? t('orders.merge.step', { n: n + 1, total }) : t('orders.merge.title'),
    plan: mergeFlowPlan(link),
    spends: [],
    shownAs: total > 1 ? `${summary} ${t('orders.merge.stepOf', { n: n + 1, total })}` : summary,
    toast: t('orders.merge.submitted', { count: link.inputs.length }),
  };
}

/** A plan that cannot run, as a flow plan carrying its errors (PlanErrors shows them). */
export function mergeFailure(plan: Pick<MergePlan, 'issues'>): CancelPlan {
  return { ok: false, issues: plan.issues, request: null, built: null, cancelIds: [], expected: { cancelIds: [] }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: [] };
}
