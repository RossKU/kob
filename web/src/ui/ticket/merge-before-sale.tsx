// The sell ticket's answer to TOKEN_UTXOS_FRAGMENTED: a sale takes at most the program's token inputs (3 for KOB's standard program), and
// when the largest UTXOs of the token do not cover the amount, the ticket offers to merge just enough first (kob/merge.ts, the target mode:
// until the 3 largest resulting UTXOs cover it). The order button then reads "Merge, then sell", and a note above it, in place of the error,
// states the merge transactions and approvals up front. A click runs them one after the other, each approved in the wallet
// (ui/orders/TxFlow.tsx, the same run as "Merge all"), then reads the wallet again and hands the sale back to the ticket, which plans it from
// that fresh state and opens its usual final confirmation: the price may have moved meanwhile, so the user approves the sale itself as
// always. A merge rejected or failed part way stops there; what was sent stays merged and the offer is planned again from it.
import type { JSX } from 'preact';
import { useRef, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { t, tIssue } from '../../i18n';
import { buildErrorText } from '../../i18n/build-error';
import { mergeLeaves, outpointOf, planMergeShape } from '../../kob/merge';
import type { PlanEnv, PlanIssue } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { awaitMergeOutputs, mergeFlow, planMergeFor, type MergeFlow } from '../orders/actions';
import { mergeSummary, mergeTokenOf } from '../orders/merge-model';
import { TxFlow, type FlowOutcome } from '../orders/TxFlow';

/** The issue code the offer answers. */
export const FRAGMENTED = 'TOKEN_UTXOS_FRAGMENTED';

export interface MergeBeforeSaleArgs {
  token: TokenInfo;
  ticker: string;
  pubkey: Hex | null;
  /** the ticket's environment (its token UTXOs are the wallet's, without what this page has just spent) */
  env: PlanEnv | null;
  /** the shown plan's TOKEN_UTXOS_FRAGMENTED issue (a single order of a KAS market), else undefined */
  issue: PlanIssue | undefined;
  refresh(): Promise<PlanEnv | null>;
  /** the merges are on the node and the ticket reads the wallet again: `leaves` are the outpoints of the merged UTXOs the sale may take */
  onMerged(leaves: ReadonlySet<string>): void;
  /** why the merge could not start or end, for the ticket's status line */
  onStop(text: string, raw?: string): void;
}

export interface MergeBeforeSale {
  /** the note that replaces the fragmented message, with the merge count; null when no merge can help */
  offer: string | null;
  /** the order button's tooltip while it merges first: why, and what follows the merges */
  hint: string;
  /** from the click until the merges are over (the ticket is busy) */
  running: boolean;
  start(): void;
  /** the run's progress card (a notification at the top right) */
  card: JSX.Element | null;
}

export function useMergeBeforeSale(a: MergeBeforeSaleArgs): MergeBeforeSale {
  const services = useServices();
  const [running, setRunning] = useState(false);
  const [flow, setFlow] = useState<{ key: number; heading: string; run: MergeFlow } | null>(null);
  const txids = useRef<string[]>([]);
  const mt = a.issue ? mergeTokenOf(a.token) : null;
  const needed = a.issue?.params?.needed;
  const target = typeof needed === 'bigint' ? needed : typeof needed === 'string' && /^\d+$/.test(needed) ? BigInt(needed) : null;
  // the count before the click, from the tree alone (nothing is built until the click)
  const shape = mt && a.env && a.pubkey && target !== null ? planMergeShape(mt, a.env.tokenUtxos, a.pubkey, new Set(), { target }) : null;
  const txs = shape?.ok ? shape.nodes.length : 0;
  const offer = txs > 0 ? t(txs === 1 ? 'ticket.merge.offer.one' : 'ticket.merge.offer.other', { ticker: a.ticker, count: txs }) : null;

  const stop = (text: string, raw?: string) => {
    setRunning(false);
    a.onStop(text, raw);
  };

  const start = () => {
    if (running || !mt || !a.pubkey || target === null) return;
    const pubkey = a.pubkey;
    setRunning(true);
    void (async () => {
      try {
        // planned again from the wallet as it is now (the ticket's environment may be seconds old)
        const plan = await planMergeFor(services, pubkey, mt, new Set(), { target });
        if (!plan.ok) {
          const first = plan.issues[0];
          // the wallet changed since the offer (a fill, another tab): no merge is needed any more, the ticket plans the sale again
          if (first?.code === 'merge.not-needed') {
            setRunning(false);
            void a.refresh().catch(() => null);
            return;
          }
          return stop(first ? tIssue('orders.issue', first) : t('orders.merge.unsupported'));
        }
        txids.current = [];
        setFlow({ key: Date.now(), heading: t('ticket.merge.heading', { ticker: a.ticker }), run: mergeFlow(services, pubkey, mt, plan, mergeSummary(plan, a.ticker)) });
      } catch (e) {
        const raw = e instanceof Error ? e.message : String(e);
        stop(buildErrorText(raw), raw);
      }
    })();
  };

  const done = async (o: FlowOutcome) => {
    const run = flow?.run;
    if (!run) return setRunning(false);
    if (o.submitted === run.steps.length) {
      // the sale spends the merged UTXOs: the node must list them before the ticket reads the wallet again
      if (await awaitMergeOutputs(services, run.sent, txids.current)) {
        const leaves = new Set(mergeLeaves(run.sent).map((k) => outpointOf({ transactionId: txids.current[k]!, index: run.sent[k]!.output.index })));
        await a.refresh().catch(() => null);
        setRunning(false);
        a.onMerged(leaves);
        return;
      }
      return stop(t('ticket.merge.notVisible'));
    }
    // stopped part way: what was sent stays merged, and the offer is planned again from it
    setRunning(false);
    void a.refresh().catch(() => null);
  };

  const card = flow ? (
    <TxFlow
      key={flow.key}
      heading={flow.heading}
      steps={flow.run.steps}
      next={flow.run.next}
      onStepSubmitted={(_s, r) => void txids.current.push(r.txid)}
      onDone={(o) => void done(o)}
      onClose={() => setFlow(null)}
    />
  ) : null;
  return { offer, hint: t('ticket.merge.actionHint', { max: a.issue?.params?.max ?? '' }), running, start, card };
}
