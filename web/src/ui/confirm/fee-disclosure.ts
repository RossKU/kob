// Disclosure of the fee RATE on the pre-sign screen (pure view model; the component only renders the rows and findings).
//
// The fee in KAS is already a row of the confirmation ("Network fee"); this adds what the dynamic fee policy (kob/fee-policy.ts) decided: the rate in sompi per
// gram, which urgency / node bucket it came from and how long the node expects that to take, and a clear note whenever the wallet paid the minimum rate because
// no estimate was available (or the policy is off, or the wallet could not afford more) or the rate was held back by the maximum rate / the per-transaction cap.
import { feeChoiceOf, type FeeChoice, type Urgency } from '../../kob/fee-policy';
import { formatKas } from '../../kob/units';
import { t, type Params } from '../../i18n';
import type { Finding, Row, Translate } from './confirm-model';

export interface FeeDisclosureInput {
  /** what the planner decided for this transaction (`feeChoiceOf(built)`); null for a transaction planned elsewhere */
  choice: FeeChoice | null;
  /** the rate the built transaction was built at (`built.fee.feeRate`), sompi per gram: the fallback when there is no choice */
  rate: bigint | null;
  /** the fee of the built transaction, sompi */
  fee: bigint;
}

export interface FeeDisclosure {
  rows: Row[];
  warnings: Finding[];
  info: Finding[];
}

const GROUP = { group: ',' };
const kasText = (v: bigint): string => formatKas(v, GROUP);

/** "under a second", "about 40 seconds", "about 5 minutes", "about 2 hours" */
export function timeText(seconds: number, tr: Translate = t): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '';
  if (seconds < 1) return tr('fees.time.subSecond');
  if (seconds < 90) return tr('fees.time.seconds', { n: Math.round(seconds) });
  if (seconds < 5_400) return tr('fees.time.minutes', { n: Math.round(seconds / 60) });
  return tr('fees.time.hours', { n: Math.round(seconds / 3_600) });
}

/** The bucket a source of urgency reads, as words ("fast: the node's priority bucket"). */
export const urgencyText = (u: Urgency, tr: Translate = t): string => tr(`fees.urgency.${u}`);

const finding = (code: string, severity: Finding['severity'], text: string, params?: Params): Finding => ({ code, severity, message: code, ...(params ? { params } : {}), text });

/** Rows and findings for the fee rate of one transaction. Never throws; an empty disclosure when there is nothing to say (no rate known). */
export function feeDisclosure(i: FeeDisclosureInput, tr: Translate = t): FeeDisclosure {
  const out: FeeDisclosure = { rows: [], warnings: [], info: [] };
  const c = i.choice;
  const rate = c ? c.rate : i.rate;
  if (rate === null) return out;
  const rateText = tr('fees.rate.value', { rate: rate.toString() });

  if (!c) {
    out.rows.push({ id: 'fee-rate', label: tr('fees.rate.label'), value: rateText, tone: 'normal' });
    return out;
  }

  // the rate row: what and where it came from
  let detail: string;
  if (c.source === 'estimate') {
    const bucket = c.bucketFeerate !== undefined ? tr('fees.rate.bucket', { feerate: roundTo(c.bucketFeerate, 2) }) : '';
    detail = [urgencyText(c.urgency, tr), bucket].filter(Boolean).join(' · ');
  } else {
    detail = tr('fees.rate.floorDetail');
  }
  out.rows.push({ id: 'fee-rate', label: tr('fees.rate.label'), value: rateText, detail, tone: c.source === 'floor' && c.reason !== 'disabled' ? 'warn' : 'normal' });

  // the node's time estimate (for the bucket the rate came from)
  if (c.source === 'estimate' && c.estimatedSeconds !== undefined) {
    const time = timeText(c.estimatedSeconds, tr);
    if (time) out.rows.push({ id: 'fee-speed', label: tr('fees.speed.label'), value: tr('fees.speed.value', { time }), tone: 'normal' });
  }

  // notes
  if (c.source === 'floor') {
    if (c.reason === 'disabled') out.info.push(finding('fee-policy-off', 'info', tr('fees.note.off', { rate: rate.toString() })));
    else if (c.reason === 'funds') out.warnings.push(finding('fee-floor-funds', 'warning', tr('fees.note.funds', { rate: rate.toString() })));
    else out.warnings.push(finding('fee-estimate-unavailable', 'warning', tr('fees.note.unavailable', { rate: rate.toString() })));
  }
  if (c.clamped && c.clampedTo === 'maxRate' && c.bucketFeerate !== undefined) {
    out.info.push(finding('fee-max-rate', 'info', tr('fees.note.maxRate', { feerate: roundTo(c.bucketFeerate, 2), max: c.cappedFrom !== undefined ? c.cappedFrom.toString() : c.rate.toString(), rate: rate.toString() })));
  }
  if (c.cappedFrom !== undefined) {
    out.info.push(finding('fee-total-cap', 'info', tr('fees.note.cap', { max: kasText(c.maxFeeSompi), from: c.cappedFrom.toString(), rate: rate.toString() })));
  }
  if (c.overCap) {
    out.warnings.push(finding('fee-over-cap', 'warning', tr('fees.note.overCap', { fee: kasText(c.fee), max: kasText(c.maxFeeSompi), rate: rate.toString() })));
  }
  return out;
}

/** The disclosure of a built transaction (its remembered fee choice, else just its rate). */
export function feeDisclosureOf(built: { fee: { fee: string; feeRate: string } }, tr: Translate = t): FeeDisclosure {
  let rate: bigint | null = null;
  try {
    rate = BigInt(built.fee.feeRate);
  } catch {
    rate = null;
  }
  return feeDisclosure({ choice: feeChoiceOf(built), rate, fee: BigInt(built.fee.fee) }, tr);
}

const roundTo = (n: number, digits: number): string => {
  const f = 10 ** digits;
  return String(Math.round(n * f) / f);
};
