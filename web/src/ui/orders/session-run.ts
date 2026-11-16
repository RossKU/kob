// One merge run under a wallet session at a time, page-wide (TxFlow.tsx): the private-key signer holds one session, and a second run opening
// its own would end the first one's. A second run is declined until the first is over.
let running: object | null = null;

/** Claims the one session run: the release (idempotent), or null while another run holds it. */
export function claimSessionRun(): (() => void) | null {
  if (running) return null;
  const me = {};
  running = me;
  return () => {
    if (running === me) running = null;
  };
}
