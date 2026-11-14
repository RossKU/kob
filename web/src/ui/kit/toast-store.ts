// Toast queue (pure: no DOM, no timers). The `<ToastHost>` component subscribes and owns the expiry timers.
export type ToastTone = 'info' | 'ok' | 'warn' | 'error';

export interface ToastItem {
  id: number;
  tone: ToastTone;
  message: string;
  /** 0 = stays until dismissed */
  timeoutMs: number;
  /** identical messages are merged; the host restarts the timer when this changes */
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
  /** how many flow cards are on screen (`hold`) */
  private holds = 0;
  private readonly heldErrors = new Set<(message: string) => void>();

  /**
   * A flow's card is on screen (founder 2026-11-14): until the returned release is called, a new toast is not shown. A notification is in the
   * bell already; an error goes to the bell's listeners (`onHeldError`), which colour it. Nothing is replayed once the card goes.
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

  /** Called with the text of every error toast held back while a flow runs (the bell takes it). */
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
   * Adds a toast (or bumps the counter of an identical one). Errors stay until dismissed. Returns the toast id, or 0 when a flow's card holds
   * the toasts back (`hold`).
   */
  push(input: ToastInput): number {
    const tone = input.tone ?? 'info';
    if (this.holds > 0) {
      if (tone === 'error') for (const l of [...this.heldErrors]) l(input.message);
      return 0;
    }
    const same = this.items.find((t) => t.tone === tone && t.message === input.message);
    if (same) {
      this.items = this.items.map((t) => (t.id === same.id ? { ...t, count: t.count + 1 } : t));
      this.emit();
      return same.id;
    }
    const item: ToastItem = { id: this.nextId++, tone, message: input.message, timeoutMs: input.timeoutMs ?? DEFAULT_TIMEOUT[tone], count: 1 };
    // the oldest non-error toast makes room first; only if all are errors the oldest error goes
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

/** The app-wide store used by `showToast`. */
export const toasts = new ToastStore();

export const showToast = (message: string, tone: ToastTone = 'info', timeoutMs?: number): number =>
  toasts.push({ message, tone, ...(timeoutMs !== undefined ? { timeoutMs } : {}) });
