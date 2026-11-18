// The sell ticket's answer to TOKEN_UTXOS_FRAGMENTED. A sale takes at most the program's token
// inputs (3 for KOB's standard program). When the largest UTXOs of the token do not cover the
// amount, the ticket offers to merge just enough first (kob/merge.ts, the target mode: until the 3
// largest resulting UTXOs cover it). The order button then reads "Merge, then sell", and a note
// above it, in place of the error, states the merge transactions and approvals up front.
//
// A click runs the merges one after the other, each approved in the wallet (ui/orders/TxFlow.tsx,
// the same run as "Merge all"). It then reads the wallet again and hands the sale back to the
// ticket, which plans it from that fresh state and opens its usual final confirmation. The price
// may have moved meanwhile, so the user approves the sale itself as always. A merge rejected or
// failed part way stops there. What was sent stays merged and the offer is planned again from it.
//
// The same run answers TOKEN_UTXOS_MIXED_BORROW: the wallet holds the amount, but in UTXOs of
// several KCC-20 borrow settings, and one sale spends one setting only. "Convert, then sell"
// converts just the settings the plain UTXOs need into plain ones (kob/merge.ts, `convert`), each
// conversion approved in the wallet, and then the sale is planned again the same way.
import type { JSX } from 'preact';
import { useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { t, tIssue } from '../../i18n';
import { buildErrorText } from '../../i18n/build-error';
import { mergeLeaves, outpointOf, planMergeShape } from '../../kob/merge';
import type { PlanEnv, PlanIssue } from '../../kob/plan-types';
import type { TokenInfo } from '../../kob/registry';
import type { Hex } from '../../kob/types';
import { awaitMergeOutputs, mergeFlow, planMergeFor, type MergeFlow } from '../orders/actions';
import { mergeApprovedOnce, mergeRunLine, mergeTokenOf } from '../orders/merge-model';
import { TxFlow, type FlowOutcome } from '../orders/TxFlow';

/**
 * The issue codes the offer answers: too many UTXOs for one sale (a merge), or no one borrow
 * setting holding the amount (a conversion).
 */
export const FRAGMENTED = 'TOKEN_UTXOS_FRAGMENTED';
export const MIXED_BORROW = 'TOKEN_UTXOS_MIXED_BORROW';
export const MERGE_ANSWERS: ReadonlySet<string> = new Set([FRAGMENTED, MIXED_BORROW]);

/** The texts of the run: a merge, or a conversion to plain. */
const MERGE_KEYS = { one: 'ticket.merge.offer.one', other: 'ticket.merge.offer.other', heading: 'ticket.merge.heading', hint: 'ticket.merge.actionHint', action: 'ticket.merge.action' };
const CONVERT_KEYS = { one: 'ticket.convert.offer.one', other: 'ticket.convert.offer.other', heading: 'ticket.convert.heading', hint: 'ticket.convert.actionHint', action: 'ticket.convert.action' };

export interface MergeBeforeSaleArgs {
  token: TokenInfo;
  ticker: string;
  pubkey: Hex | null;
  /**
   * The ticket's environment. Its token UTXOs are the wallet's, without what this page has just
   * spent.
   */
  env: PlanEnv | null;
  /**
   * The shown plan's TOKEN_UTXOS_FRAGMENTED or TOKEN_UTXOS_MIXED_BORROW issue (a single order of a
   * KAS market), else undefined.
   */
  issue: PlanIssue | undefined;
  refresh(): Promise<PlanEnv | null>;
  /**
   * The merges are on the node and the ticket reads the wallet again. `leaves` are the outpoints of
   * the merged UTXOs the sale may take.
   */
  onMerged(leaves: ReadonlySet<string>): void;
  /** Why the merge could not start or end, for the ticket's status line. */
  onStop(text: string, raw?: string): void;
}

export interface MergeBeforeSale {
  /**
   * The note that replaces the fragmented message, with the merge count. Null when no merge can
   * help.
   */
  offer: string | null;
  /** The order button's tooltip while it merges first: why, and what follows the merges. */
  hint: string;
  /** The order button's label while it merges (or converts) first. */
  action: string;
  /** The button's text while the merges (or conversions) run. */
  busy: string;
  /** From the click until the merges are over (the ticket is busy). */
  running: boolean;
  start(): void;
  /** The run's progress card (a notification at the top right). */
  card: JSX.Element | null;
}

export function useMergeBeforeSale(a: MergeBeforeSaleArgs): MergeBeforeSale {
  const services = useServices();
  const wallet = useWallet();
  const [running, setRunning] = useState(false);
  const [flow, setFlow] = useState<{ key: number; heading: string; run: MergeFlow } | null>(null);
  const txids = useRef<string[]>([]);
  const mt = a.issue ? mergeTokenOf(a.token) : null;
  // No one borrow setting holds the amount, so the run converts to plain instead of merging.
  const convert = a.issue?.code === MIXED_BORROW;
  // What a running run is. Its texts stay while the ticket plans again around it.
  const ran = useRef(convert);
  if (!running && !flow) ran.current = convert;
  const needed = a.issue?.params?.needed;
  const target = typeof needed === 'bigint' ? needed : typeof needed === 'string' && /^\d+$/.test(needed) ? BigInt(needed) : null;
  // The count before the click, from the tree alone. Nothing is built until the click.
  const shape = mt && a.env && a.pubkey && target !== null ? planMergeShape(mt, a.env.tokenUtxos, a.pubkey, new Set(), { target, convert }) : null;
  const txs = shape?.ok ? shape.nodes.length : 0;
  // The private-key signer approves the whole merge once (its session). Every other wallet approves
  // once per transaction.
  const once = !!mt && mergeApprovedOnce(wallet.adapter, mt, convert);
  const k = convert ? CONVERT_KEYS : MERGE_KEYS;
  const offer = txs > 0 ? t(txs === 1 ? k.one : once ? 'ticket.merge.offer.once' : k.other, { ticker: a.ticker, count: txs }) : null;

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
        // Planned again from the wallet as it is now. The ticket's environment may be seconds old.
        const plan = await planMergeFor(services, pubkey, mt, new Set(), { target, convert });
        if (!plan.ok) {
          const first = plan.issues[0];
          // The wallet changed since the offer (a fill, another tab). No merge is needed any more,
          // and the ticket plans the sale again.
          if (first?.code === 'merge.not-needed') {
            setRunning(false);
            void a.refresh().catch(() => null);
            return;
          }
          return stop(first ? tIssue('orders.issue', first) : t('orders.merge.unsupported'));
        }
        txids.current = [];
        setFlow({ key: Date.now(), heading: t(k.heading, { ticker: a.ticker }), run: mergeFlow(services, pubkey, mt, plan, mergeRunLine(plan)) });
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
      // The sale spends the merged UTXOs, so the node must list them before the ticket reads the
      // wallet again.
      if (await awaitMergeOutputs(services, run.sent, txids.current)) {
        const leaves = new Set(mergeLeaves(run.sent).map((k) => outpointOf({ transactionId: txids.current[k]!, index: run.sent[k]!.output.index })));
        await a.refresh().catch(() => null);
        setRunning(false);
        a.onMerged(leaves);
        return;
      }
      return stop(t('ticket.merge.notVisible'));
    }
    // Stopped part way. What was sent stays merged, and the offer is planned again from it.
    setRunning(false);
    void a.refresh().catch(() => null);
  };

  const card = flow ? (
    <TxFlow
      key={flow.key}
      heading={flow.heading}
      steps={flow.run.steps}
      next={flow.run.next}
      // The sale follows the merges in the ticket's own confirmation. The stepper shows it last.
      then={{ label: t('stepper.step.sell'), note: t('stepper.note.confirmSale') }}
      {...(flow.run.session ? { session: flow.run.session } : {})}
      onStepSubmitted={(_s, r) => void txids.current.push(r.txid)}
      onDone={(o) => void done(o)}
      onClose={() => setFlow(null)}
    />
  ) : null;
  return { offer, hint: t(k.hint, { max: a.issue?.params?.max ?? '' }), action: t(k.action), busy: t(ran.current ? 'ticket.convert.busy' : 'ticket.merge.busy'), running, start, card };
}
