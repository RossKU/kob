// Toast queue (pure: no DOM, no timers). The `<ToastHost>` component subscribes and owns the expiry
// timers.
export type ToastTone = 'info' | 'ok' | 'warn' | 'error';

export interface ToastItem {
  id: number;
  tone: ToastTone;
  message: string;
  /** 0 means the toast stays until dismissed. */
  timeoutMs: number;
  /** Identical messages are merged. The host restarts the timer when this changes. */
  count: number;
}

export interface ToastInput {
  tone?: ToastTone;
  message: string;
  timeoutMs?: number;
}

export const MAX_TOASTS = 5;
export const DEFAULT_TIMEOUT: Record<ToastTone, number> = { info: 4000, ok: 4000, warn: 8000, error: 0 };

export class ToastStore {
  private items: ToastItem[] = [];
  private nextId = 1;
  private readonly listeners = new Set<() => void>();
  /** How many flow cards are on screen (`hold`). */
  private holds = 0;
  private readonly heldErrors = new Set<(message: string) => void>();

  /**
   * A flow's card is on screen. Until the returned release is called, a new toast that is not an
   * error is not shown. A notification is in the bell already, and nothing is replayed once the
   * card goes. An error is shown all the same, because a failure is always seen at the top right
   * and never only in the bell. It also goes to the bell's listeners (`onHeldError`), which
   * colour it.
   */
  hold(): () => void {
    this.holds++;
    let done = false;
    return () => {
      if (done) return;
      done = true;
      this.holds = Math.max(0, this.holds - 1);
    };
  }

  /** Whether a flow's card holds the toasts back. */
  get held(): boolean {
    return this.holds > 0;
  }

  /**
   * Called with the text of every error toast raised while a flow's card is on screen. The bell
   * lists it too.
   */
  onHeldError(fn: (message: string) => void): () => void {
    this.heldErrors.add(fn);
    return () => this.heldErrors.delete(fn);
  }

  snapshot(): readonly ToastItem[] {
    return this.items;
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit(): void {
    for (const l of [...this.listeners]) l();
  }

  /**
   * Adds a toast, or bumps the counter of an identical one. Errors stay until dismissed. Returns
   * the toast id, or 0 when a flow's card holds the toasts back (`hold`), which never holds an
   * error.
   */
  push(input: ToastInput): number {
    const tone = input.tone ?? 'info';
    if (this.holds > 0) {
      if (tone !== 'error') return 0;
      for (const l of [...this.heldErrors]) l(input.message);
    }
    const same = this.items.find((t) => t.tone === tone && t.message === input.message);
    if (same) {
      this.items = this.items.map((t) => (t.id === same.id ? { ...t, count: t.count + 1 } : t));
      this.emit();
      return same.id;
    }
    const item: ToastItem = { id: this.nextId++, tone, message: input.message, timeoutMs: input.timeoutMs ?? DEFAULT_TIMEOUT[tone], count: 1 };
    // The oldest non-error toast makes room first. Only if all are errors does the oldest error go.
    let next = [...this.items, item];
    while (next.length > MAX_TOASTS) {
      const victim = next.findIndex((t) => t.tone !== 'error' && t.id !== item.id);
      next.splice(victim >= 0 ? victim : 0, 1);
    }
    this.items = next;
    this.emit();
    return item.id;
  }

  dismiss(id: number): void {
    const next = this.items.filter((t) => t.id !== id);
    if (next.length === this.items.length) return;
    this.items = next;
    this.emit();
  }

  clear(): void {
    if (!this.items.length) return;
    this.items = [];
    this.emit();
  }
}

/**
 * What the host shows (kit/Toast.tsx): the toasts on screen, how many wait behind them (a phone's
 * "+N" chip) and whether an error is among those.
 */
export interface HostView {
  shown: readonly ToastItem[];
  waiting: number;
  waitingError: boolean;
}

/**
 * What the host shows as a list.
 * - A desktop or a tablet shows every toast, stacked under the cards (`cards`: a send's or a
 *   flow's card other components put in the host).
 * - A phone shows one notification at a time: a card first, with the toasts waiting behind it,
 *   else the newest error, else the newest toast. The rest, and the cards beyond the first, wait
 *   behind the chip, which is red while an error is among them.
 */
export function hostView(list: readonly ToastItem[], cards: number, phone: boolean): HostView {
  if (!phone) return { shown: list, waiting: 0, waitingError: false };
  const error = [...list].reverse().find((x) => x.tone === 'error');
  const top = cards > 0 ? null : (error ?? list.at(-1) ?? null);
  const behind = list.filter((x) => x !== top);
  return { shown: top ? [top] : [], waiting: Math.max(0, cards - 1) + behind.length, waitingError: behind.some((x) => x.tone === 'error') };
}

/** The app-wide store used by `showToast`. */
export const toasts = new ToastStore();

export const showToast = (message: string, tone: ToastTone = 'info', timeoutMs?: number): number =>
  toasts.push({ message, tone, ...(timeoutMs !== undefined ? { timeoutMs } : {}) });
