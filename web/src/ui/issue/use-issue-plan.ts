// Debounced planning of the issuance: form and funding to `planIssue`. A newer form cancels the
// timer of the older one. A plan is only ever offered for the exact inputs it was made from
// (`key`), so a stale plan can never reach the review button.
import { useEffect, useState } from 'preact/hooks';
import { pickFor, type FeeContext } from '../../kob/fee-policy';
import { IssueFormError, planIssue, type IssueForm, type IssuePlan } from '../../kob/issue';
import type { PlanIssue } from '../../kob/plan-types';
import type { Hex, KeyUtxo } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';

/** The planning state: the plan for the current inputs, or why there is none. */
export type IssuePlanState =
  | { status: 'idle' }
  | { status: 'pending' }
  | { status: 'ready'; plan: IssuePlan }
  | { status: 'invalid'; issues: PlanIssue[] }
  | { status: 'error'; message: string };

export const PLAN_DEBOUNCE_MS = 300;

/** The inputs of the issuance plan. */
export interface UseIssuePlanInput {
  kob: KobWasm;
  form: IssueForm;
  maker: Hex | null;
  network: string;
  /** Null means not loaded, or no wallet, so there is nothing to plan. */
  funding: KeyUtxo[] | null;
  /**
   * The fee policy and the node's estimate. Null means not read yet, and nothing is planned until
   * it is. The policy never needs more than one short read.
   */
  fees: FeeContext | null;
  registryTickers: string[];
  /**
   * False while the form has errors of its own or the wallet cannot sign. Nothing is planned then.
   */
  enabled: boolean;
  /** Bump to plan again after an error. */
  retry: number;
}

/**
 * Stable identity of the planning inputs. Funding counts by outpoint and amount, so a spent UTXO
 * changes it.
 */
export function planKey(i: Pick<UseIssuePlanInput, 'form' | 'maker' | 'network' | 'funding' | 'fees' | 'registryTickers' | 'retry'>): string {
  // The fee enters through the rate it picks. A changed estimate that picks the same rate does not
  // replan.
  const rate = i.fees ? pickFor(i.fees, 'normal').rate.toString() : '';
  return JSON.stringify([i.form, i.maker, i.network, i.registryTickers, i.retry, rate, (i.funding ?? []).map((u) => `${u.transactionId}:${u.index}:${u.amount}`)]);
}

/** Plans the issuance after a debounce and returns the state for the current inputs. */
export function useIssuePlan(input: UseIssuePlanInput): IssuePlanState {
  const key = planKey(input);
  const active = input.enabled && input.maker !== null && input.funding !== null && input.fees !== null;
  const [done, setDone] = useState<{ key: string; state: IssuePlanState } | null>(null);
  useEffect(() => {
    if (!active) return;
    const timer = setTimeout(() => {
      let state: IssuePlanState;
      try {
        const plan = planIssue(input.kob, input.form, {
          funding: input.funding as KeyUtxo[],
          maker: input.maker as Hex,
          network: input.network,
          registryTickers: input.registryTickers,
          ...(input.fees ? { fees: input.fees } : {}),
        });
        state = { status: 'ready', plan };
      } catch (e) {
        if (e instanceof IssueFormError) state = { status: 'invalid', issues: e.issues };
        else state = { status: 'error', message: e instanceof Error ? e.message : String(e) };
      }
      setDone({ key, state });
    }, PLAN_DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // `key` covers every input of the plan.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, active]);
  if (!active) return { status: 'idle' };
  return done !== null && done.key === key ? done.state : { status: 'pending' };
}
