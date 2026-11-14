// "Merge all" in My orders > Balances and the sell ticket's merge: which token UTXOs a merge may take, how many per token, the flow steps of a
// planned merge and the summary its confirmation shows. One click plans the whole tree (kob/merge.ts): its confirmation states the transactions,
// the wallet approvals (one per transaction, one after the other) and the KAS freed, and nothing is sent before the user confirms. Pure.
import type { OrderView, StrayView } from '../../data/indexer-types';
import { t } from '../../i18n';
import type { CancelPlan, OrderSnapshot } from '../../kob/cancel';
import { mergeCandidates, outpointOf, planMergeShape, type MergeLink, type MergePlan, type MergeToken } from '../../kob/merge';
import { DEFAULT_CARRIER } from '../../kob/orders/common';
import { formatKas } from '../../kob/order-facts';
import type { TokenInfo } from '../../kob/registry';
import type { KobWasm } from '../../kob/wasm';
import { extensionOfState, familyOfProgram } from '../../kob/token-state';
import type { Hex, TokenProgram, TokenUtxo } from '../../kob/types';
import { countText } from './plural';
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

/** What a token outside the registry is merged with: the build's templates (slots, template hash) and the program the node verified its UTXOs under (balances-data.ts `programOf`). */
export interface UnlistedMerge {
  kob: Pick<KobWasm, 'templates'>;
  programs: ReadonlyMap<Hex, TokenProgram>;
}

/**
 * The merge token of a token the registry does not list (an open-list or foreign KCC-20 / KRON the wallet holds): its program is the node-verified one, its template
 * hash and input slots are those of that program's pinned template, and its extension commitment is the one the wallet's own plain UTXOs of it share (the one most of them
 * carry when they differ: `mergeCandidates` then takes only those). Null without a verified program, a pinned template or a UTXO to merge.
 */
export function unlistedMergeToken(
  id: Hex, freeTokens: readonly TokenUtxo[], maker: Hex, reserved: ReadonlySet<string>, unlisted: UnlistedMerge,
): MergeToken | null {
  const program = unlisted.programs.get(id);
  const tpl = program ? unlisted.kob.templates().find((x) => x.name === program && x.tokenSlots) : undefined;
  if (!program || !tpl?.tokenSlots) return null;
  const base: MergeToken = { covenantId: id, program, templateHash: tpl.hash, extensionCommitment: null, slots: { inputs: tpl.tokenSlots[0] } };
  if (familyOfProgram(program) === 'kron') return base;
  // candidates under any extension, grouped by it: the commitment held by the most of them (ties: the smaller one, so the choice never flips)
  const byExt = new Map<Hex, number>();
  for (const u of mergeCandidates(freeTokens, base, maker, reserved)) {
    const ext = extensionOfState(u.state);
    if (ext) byExt.set(ext, (byExt.get(ext) ?? 0) + 1);
  }
  const best = [...byExt].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))[0];
  return best ? { ...base, extensionCommitment: best[0] } : null;
}

/** The token to merge for a held token id: the registry's (unchanged), else, when `unlisted` is given, the one built from its node-verified program and the wallet's UTXOs. */
export function mergeTokenFor(
  id: Hex, freeTokens: readonly TokenUtxo[], tokens: ReadonlyMap<Hex, TokenInfo>, maker: Hex, reserved: ReadonlySet<string>, unlisted?: UnlistedMerge,
): MergeToken | null {
  const info = tokens.get(id);
  if (info) return mergeTokenOf(info);
  return unlisted ? unlistedMergeToken(id, freeTokens, maker, reserved, unlisted) : null;
}

/**
 * Per token: how many of the wallet's free UTXOs a merge may take (tokens with fewer than two are left out). `unlisted`: tokens outside the registry are merged too,
 * under their node-verified program (`unlistedMergeToken`).
 */
export function mergeableCounts(freeTokens: readonly TokenUtxo[], tokens: ReadonlyMap<Hex, TokenInfo>, maker: Hex, reserved: ReadonlySet<string>, unlisted?: UnlistedMerge): Map<Hex, number> {
  const out = new Map<Hex, number>();
  for (const id of new Set(freeTokens.map((u) => u.covenantId).filter((x): x is Hex => !!x))) {
    const mt = mergeTokenFor(id, freeTokens, tokens, maker, reserved, unlisted);
    if (!mt) continue;
    const n = mergeCandidates(freeTokens, mt, maker, reserved).length;
    if (n >= 2) out.set(id, n);
  }
  return out;
}

/**
 * Per token: about how much KAS "Merge all" frees (before its fees, a few hundredths of a KAS per transaction): the KAS on the UTXOs the
 * tree merges (planMergeShape: every mergeable one, up to MAX_MERGE_TXS transactions) less the carrier the final UTXO keeps (the largest
 * input's, at most the default carrier: kob/merge.ts `mergedCarrier`; a carrier an intermediate output keeps is freed by the next level).
 * Tokens with fewer than two mergeable UTXOs are left out. The confirmation states the exact amount.
 */
export function mergeAllFrees(freeTokens: readonly TokenUtxo[], tokens: ReadonlyMap<Hex, TokenInfo>, maker: Hex, reserved: ReadonlySet<string>, unlisted?: UnlistedMerge): Map<Hex, bigint> {
  const out = new Map<Hex, bigint>();
  for (const id of new Set(freeTokens.map((u) => u.covenantId).filter((x): x is Hex => !!x))) {
    const mt = mergeTokenFor(id, freeTokens, tokens, maker, reserved, unlisted);
    if (!mt) continue;
    const shape = planMergeShape(mt, freeTokens, maker, reserved);
    if (!shape.ok) continue;
    const kas = shape.nodes.flatMap((n) => n.fresh.map((u) => BigInt(u.amount)));
    const largest = kas.reduce((m, v) => (v > m ? v : m), 0n);
    const kept = largest < DEFAULT_CARRIER ? largest : DEFAULT_CARRIER;
    out.set(id, kas.reduce((s, v) => s + v, 0n) - kept);
  }
  return out;
}

/** "3 transactions, one after the other, each approved in your wallet (3 approvals)": what a merge of `txs` transactions asks of the user. */
const mergeRunText = (txs: number): string =>
  t(txs === 1 ? 'orders.merge.run.one' : 'orders.merge.run.other', { txs: countText('tx', txs), approvals: countText('approval', txs) });

/**
 * The merge in a few sentences (shown on its confirmation, and while it runs): what it merges, the transactions and the wallet approvals it
 * takes (one each, one after the other), the KAS it frees (the merged UTXOs' carriers, less the ones the outputs keep, less the fees: the
 * reason to merge at all; when the fees exceed the freed carriers, the net cost instead) and how many mergeable UTXOs a full merge leaves. The
 * sell ticket's merge (`target`) says the sale comes after it, with its own confirmation.
 */
export function mergeSummary(plan: Pick<MergePlan, 'before' | 'after' | 'merged' | 'freed' | 'links' | 'fee' | 'target'>, ticker: string): string {
  const n = plan.links.length;
  const counts = { ticker, run: mergeRunText(n), merged: plan.merged, before: plan.before };
  const head =
    plan.target !== null
      ? t('orders.merge.summaryTarget', counts)
      : plan.merged === plan.before
        ? t('orders.merge.summaryAll', counts)
        : t('orders.merge.summarySome', counts);
  const kas =
    plan.freed > 0n
      ? t('orders.merge.freed', { freed: formatKas(plan.freed), fee: formatKas(plan.fee) })
      : t('orders.merge.costs', { cost: formatKas(-plan.freed), fee: formatKas(plan.fee) });
  if (plan.target !== null) return `${head} ${kas}`;
  const rest = plan.after >= 2 ? t('orders.merge.more', { after: plan.after }) : t('orders.merge.last');
  return `${head} ${kas} ${rest}`;
}

/** A link as a flow plan (TxFlow takes the built transaction and the claims to check). */
export function mergeFlowPlan(link: MergeLink): CancelPlan {
  return { ok: true, issues: link.issues, request: null, built: link.built, cancelIds: [], expected: { cancelIds: [] }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: link.fundingUsed };
}

/**
 * Link `n` (0-based) of `total` as a flow step, `summary` shown while it runs. The user confirmed the whole run before it started, so the
 * step is `preconfirmed`: the wallet's approval is its one prompt. A run of several reports in its card, not with a toast per step.
 */
export function mergeStep(link: MergeLink, n: number, total: number, summary: string): FlowStep {
  return {
    title: total > 1 ? t('orders.merge.step', { n: n + 1, total }) : t('orders.merge.title'),
    plan: mergeFlowPlan(link),
    spends: [],
    shownAs: summary,
    toast: t('orders.merge.submitted', { count: link.inputs.length }),
    ...(total > 1 ? { silent: true } : {}),
    preconfirmed: true,
  };
}

/** A plan that cannot run, as a flow plan carrying its errors (PlanErrors shows them). */
export function mergeFailure(plan: Pick<MergePlan, 'issues'>): CancelPlan {
  return { ok: false, issues: plan.issues, request: null, built: null, cancelIds: [], expected: { cancelIds: [] }, tokensReturned: 0n, kasReleased: 0n, fundingUsed: [] };
}
