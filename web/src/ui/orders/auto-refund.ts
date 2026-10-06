// Automatic refund of the wallet's own expired orders while the app is open (C5 W-15, matcher.md 10.10: "the wallet refunds itself when online").
// Opt-in (off by default, `kob.autoRefund.v1`). A refund needs NO signature: the covenant's `refund()` pays the whole order back to its maker's key
// once its refund time has come (expiry / day-order end / 90 days idle / IOC-FOK kill), and anyone may submit it; keepers do it for the refund tip.
// The wallet does it itself so a day order or an IOC rest comes back without waiting for a keeper (and keeps the tip).
//
// Safety: only plans that need no signature at all are submitted (a refund whose own value cannot pay the fee needs a funding input, i.e. a
// signature: that one is left to the Refund button and its confirmation screen); the transaction is finalized and script-validated locally before it
// is sent, and every output must pay the connected wallet key (the decoder's expectations: nothing leaves the wallet). One attempt per order per session.
import { useEffect, useRef } from 'preact/hooks';
import { buildCancelEnv } from '../../app/env';
import type { Services } from '../../app/services';
import type { OrderView } from '../../data/indexer-types';
import { t } from '../../i18n';
import { planRefund, type CancelPlan } from '../../kob/cancel';
import { decodeSigning } from '../../kob/decode';
import type { RecordStore } from '../../kob/records';
import type { Hex } from '../../kob/types';
import { showToast } from '../kit';
import { markCancelling, snapshotFor } from './actions';
import { loadMakerOrders } from './orders-data';

export const AUTO_REFUND_KEY = 'kob.autoRefund.v1';
export const AUTO_REFUND_POLL_MS = 60_000;

interface StorageLike { getItem(k: string): string | null; setItem(k: string, v: string): void }
const storage = (): StorageLike | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/** The opt-in (default off). Never throws. */
export function loadAutoRefund(s: StorageLike | null = storage()): boolean {
  try {
    return s?.getItem(AUTO_REFUND_KEY) === '1';
  } catch {
    return false;
  }
}

export function saveAutoRefund(on: boolean, s: StorageLike | null = storage()): void {
  try {
    s?.setItem(AUTO_REFUND_KEY, on ? '1' : '0');
  } catch {
    /* storage blocked: the choice lasts for this page only */
  }
}

/** Live own orders whose refund time has come on the indexer's clock (refund_due_daa <= current_daa). */
export function refundDue(views: readonly OrderView[]): OrderView[] {
  return views.filter((v) => (v.status === 'open' || v.status === 'partial') && v.state_known && v.refund_due_daa != null && v.current_daa != null && v.current_daa >= v.refund_due_daa);
}

/** Whether a refund plan may be sent without asking: built, no signature, nothing blocking in the pre-sign decoder. */
export function submittableWithoutSignature(s: Pick<Services, 'kob' | 'registry'>, pubkey: Hex, plan: CancelPlan): boolean {
  if (!plan.ok || !plan.built || plan.built.sign.length > 0) return false;
  const d = decodeSigning({ kob: s.kob, built: plan.built, maker: pubkey, registry: s.registry, expected: plan.expected });
  return d.blocking.length === 0;
}

/** One pass: refunds every due own order once (`tried` remembers the attempts of this session). Returns the submitted transaction ids. */
export async function autoRefundPass(s: Services, pubkey: Hex, store: RecordStore, tried: Set<Hex>, signal?: AbortSignal): Promise<Hex[]> {
  if (!s.indexer) return [];
  const views = await loadMakerOrders(s.indexer, pubkey, signal ? { signal } : {});
  const due = refundDue(views).filter((v) => !tried.has(v.covenant_id));
  if (!due.length) return [];
  const env = await buildCancelEnv(s, { pubkey });
  const out: Hex[] = [];
  for (const v of due) {
    tried.add(v.covenant_id);
    try {
      const snap = await snapshotFor(s, { id: v.covenant_id, view: v, record: null, resolved: null }, []);
      // no reclaim: a signature-free refund (the order's own value pays the fee); funding would need the wallet's signature
      const plan = planRefund({ ...env, funding: [] }, snap);
      if (!submittableWithoutSignature(s, pubkey, plan)) continue;
      const signed = s.kob.finalize(plan.built!, [], { tightenBudgets: true });
      s.kob.validate(signed);
      const txid = await s.node.submitTransaction(signed.tx);
      if (txid !== signed.tx.id) continue;
      await markCancelling(store, plan, txid);
      out.push(txid);
    } catch {
      /* a refund a keeper won, a node refusal, an order that moved: the Refund button stays available */
    }
  }
  return out;
}

/** Runs the passes while `enabled` (the opt-in), a wallet on the app's network is connected and the indexer is configured. */
export function useAutoRefund(s: Services, pubkey: Hex | null, store: RecordStore, enabled: boolean): void {
  const tried = useRef(new Set<Hex>());
  useEffect(() => {
    tried.current = new Set();
  }, [pubkey]);
  useEffect(() => {
    if (!enabled || !pubkey || !s.indexer) return;
    const ctl = new AbortController();
    let busy = false;
    const run = async () => {
      if (busy) return;
      busy = true;
      try {
        const sent = await autoRefundPass(s, pubkey, store, tried.current, ctl.signal);
        if (sent.length) showToast(t('orders.autoRefund.done', { count: sent.length }), 'ok');
      } catch {
        /* the indexer or node is away: the next pass tries again */
      } finally {
        busy = false;
      }
    };
    void run();
    const id = setInterval(() => void run(), AUTO_REFUND_POLL_MS);
    return () => {
      ctl.abort();
      clearInterval(id);
    };
  }, [enabled, pubkey, s, store]);
}

/** Event fired on `window` when the opt-in changes (the toggle in My orders and the app-wide runner share it). */
export const AUTO_REFUND_EVENT = 'kob-auto-refund';

/** Saves the opt-in and tells the app-wide runner. */
export function setAutoRefund(on: boolean): void {
  saveAutoRefund(on);
  try {
    window.dispatchEvent(new Event(AUTO_REFUND_EVENT));
  } catch {
    /* no window (tests) */
  }
}
