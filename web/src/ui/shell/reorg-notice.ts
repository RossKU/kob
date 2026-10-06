// The header indicator of a DEEP chain re-organisation. Ordinary re-orgs (a few blocks of the virtual chain, many a minute on Kaspa) are handled by
// the indexer and say nothing here; what a re-org does to the user's own orders is reported through the notification centre (kob/notifications).
// The indicator is shown when the deep-re-org counter rises and hidden after `ms` (a newer one re-arms the timer) or on `dismiss`.
// Framework-free so the timing is unit-testable with fake timers; `useReorgNotice` binds it to Preact.
import { useEffect, useRef, useState } from 'preact/hooks';

export const REORG_INDICATOR_MS = 10 * 60_000;
/** Kaspa makes 10 blocks a second: a re-org that replaces more than this many chain blocks (3 s of chain) is beyond what the indexer treats as routine. */
export const DEEP_REORG_BLOCKS = 30;

/** True for a `reorg` notice deep enough for the header indicator. */
export const isDeepReorg = (revertedBlocks: number | undefined): boolean => (revertedBlocks ?? 0) > DEEP_REORG_BLOCKS;

export class ReorgNotice {
  visible = false;
  private seen = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly onChange: (visible: boolean) => void, private readonly ms = REORG_INDICATOR_MS) {}

  /** Feed the running deep-re-org count; a count above the last one seen shows the indicator and re-arms the timer. */
  update(count: number): void {
    if (count <= this.seen) return;
    this.seen = count;
    this.set(true);
    this.clear();
    this.timer = setTimeout(() => {
      this.timer = null;
      this.set(false);
    }, this.ms);
  }

  dismiss(): void {
    this.clear();
    this.set(false);
  }

  dispose(): void {
    this.clear();
  }

  private clear(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private set(v: boolean): void {
    if (this.visible === v) return;
    this.visible = v;
    this.onChange(v);
  }
}

/** Whether the deep-re-org indicator is shown, for the running deep-re-org count of the feed. */
export function useReorgNotice(reorgs: number): boolean {
  const [visible, setVisible] = useState(false);
  const ref = useRef<ReorgNotice | null>(null);
  if (!ref.current) ref.current = new ReorgNotice(setVisible);
  useEffect(() => () => ref.current?.dispose(), []);
  useEffect(() => ref.current?.update(reorgs), [reorgs]);
  return visible;
}
