// The red toast of a failed action. This module is pure. It decides which stops raise one and what
// its text is.
// A send of the order ticket that stops is said at the top right with its reason, as one short
// line "<what>: <why>". A stop can come from a check, a decline in the wallet or a refusal by the
// node.
// The app's own Cancel of its confirmation is the user's choice, not a failure, so it raises no
// toast. A flow's card (ui/orders/TxFlow.tsx) shows its own failure in red and raises none.

/** What a stopped send carries (confirm/SendStatus.tsx `SendStatus`). */
export interface StopLike {
  phase: string;
  code?: string | undefined;
  text?: string | undefined;
}

const STOPPED: ReadonlySet<string> = new Set(['blocked', 'rejected', 'failed']);

/**
 * A send's own stop that is the user's choice in the app, the Cancel of the app's final
 * confirmation. It is never a failure toast.
 */
const CHOSEN: ReadonlySet<string> = new Set(['cancelled']);

/** Whether `s` is a stopped send the user did not choose to stop. */
export function isFailure(s: StopLike | null | undefined): s is StopLike {
  return !!s && STOPPED.has(s.phase) && !CHOSEN.has(s.code ?? '');
}

/**
 * One short line "<heading>: <reason>", with the reason's spaces folded. With no reason it is the
 * heading alone.
 */
export function failureText(heading: string, reason: string | null | undefined): string {
  const why = (reason ?? '').replace(/\s+/g, ' ').trim();
  return why ? `${heading}: ${why}` : heading;
}

/**
 * The toast text of a send's status. It is null when the send raises none, that is when it is not
 * stopped or was stopped by the user's own Cancel.
 */
export function failureToast(heading: string, s: StopLike | null | undefined, fallback = ''): string | null {
  return isFailure(s) ? failureText(heading, s.text || fallback) : null;
}
