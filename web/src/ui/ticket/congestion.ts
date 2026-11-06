// The congestion note of the final confirmation (pure part): whether blocks are contended, read from the node's fee estimate at most once per CONGESTION_POLL_MS.
// The rule is `isContended` (kob/fee-policy.ts: the executor's reading of `getFeeEstimate`). The note only informs: it never changes a rate, never switches
// the Settings toggle ("Pay the priority fee") and is never asked per transaction. A node without an estimate, or one that errors, says nothing.
import { isContended, type FeeEstimate } from '../../kob/fee-policy';

/** The ticket asks the node for the estimate this often while it is shown (and the page is visible); never in the background, never on an idle page. */
export const CONGESTION_POLL_MS = 30_000;

export class CongestionProbe {
  private at = Number.NEGATIVE_INFINITY;
  private value = false;
  private inflight: Promise<boolean> | null = null;

  constructor(
    private readonly read: () => Promise<FeeEstimate | null | undefined>,
    private readonly floor: bigint,
    private readonly now: () => number = Date.now,
    private readonly everyMs: number = CONGESTION_POLL_MS,
  ) {}

  /** The last answer without a call. */
  get contended(): boolean {
    return this.value;
  }

  /**
   * Whether blocks are contended. Asks the node only when the last ask is `everyMs` old (any caller, however often it comes: a remounted ticket reuses the
   * answer); concurrent callers share one ask. Never rejects: a failed or missing estimate is "not contended".
   */
  probe(): Promise<boolean> {
    if (this.inflight) return this.inflight;
    const t = this.now();
    if (t - this.at < this.everyMs && t >= this.at) return Promise.resolve(this.value);
    this.at = t;
    this.inflight = Promise.resolve()
      .then(this.read)
      .then((e) => isContended(e, this.floor))
      .catch(() => false)
      .then((c) => {
        this.value = c;
        this.inflight = null;
        return c;
      });
    return this.inflight;
  }
}
