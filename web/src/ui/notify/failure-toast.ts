// The red toast of a failed action (founder 2026-11-17: "When something fails, the red toast doesn't appear at the top right"). Pure: which stops
// raise one and its text. A send of the order ticket that stops (blocked by a check, declined in the wallet, refused by the node) is said at the top
// right with its reason, as one short line: "<what>: <why>". The app's own Cancel of its confirmation is the user's choice, not a failure: no toast.
// A flow's card (ui/orders/TxFlow.tsx) shows its own failure in red and raises none.

/** What a stopped send carries (confirm/SendStatus.tsx `SendStatus`). */
export interface StopLike {
  phase: string;
  code?: string | undefined;
  text?: string | undefined;
}

const STOPPED: ReadonlySet<string> = new Set(['blocked', 'rejected', 'failed']);

/** A send's own stop that is the user's choice in the app (the Cancel of the app's final confirmation): never a failure toast. */
const CHOSEN: ReadonlySet<string> = new Set(['cancelled']);

/** Whether `s` is a stopped send the user did not choose to stop. */
export function isFailure(s: StopLike | null | undefined): s is StopLike {
  return !!s && STOPPED.has(s.phase) && !CHOSEN.has(s.code ?? '');
}

/** One short line: "<heading>: <reason>" (the reason's spaces folded; no reason: the heading alone). */
export function failureText(heading: string, reason: string | null | undefined): string {
  const why = (reason ?? '').replace(/\s+/g, ' ').trim();
  return why ? `${heading}: ${why}` : heading;
}

/** The toast text of a send's status, or null when it raises none (not stopped, or stopped by the user's own Cancel). */
export function failureToast(heading: string, s: StopLike | null | undefined, fallback = ''): string | null {
  return isFailure(s) ? failureText(heading, s.text || fallback) : null;
}
