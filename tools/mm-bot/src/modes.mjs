// Modes: QUOTING, IDLE (reference, jump, peg, fee-cap, cooldown) and STOPPING. Only the operator stops the bot.

export const QUOTING = 'QUOTING';
export const IDLE = 'IDLE';
export const STOPPING = 'STOPPING';

/** The UTC day of a timestamp, `YYYY-MM-DD`. */
export const utcDay = (ms) => new Date(ms).toISOString().slice(0, 10);

/** A fresh mode record: the bot starts in IDLE and must see a steady reference for the cool-down before it quotes. */
export function initialMode(nowMs) {
  return { mode: IDLE, reason: 'cooldown', since: nowMs, steadySince: null, feeDay: null };
}

/** The next mode from { stop, refOk, jump, feeBlocked, pegOk }; `pull`: cancel every quote. */
export function nextMode(prev, input, cfg, nowMs) {
  const day = utcDay(nowMs);
  const to = (mode, reason, extra = {}) => {
    const changed = mode !== prev.mode || reason !== prev.reason;
    return {
      mode,
      reason,
      since: changed ? nowMs : prev.since,
      steadySince: prev.steadySince,
      feeDay: prev.feeDay,
      ...extra,
      changed,
      pull: mode !== QUOTING,
    };
  };

  if (input.stop || prev.mode === STOPPING) return to(STOPPING, 'operator');

  // the fee cap holds until the UTC day rolls over, whatever the reference does
  if (prev.reason === 'fee-cap' && prev.feeDay === day) return to(IDLE, 'fee-cap');
  if (input.feeBlocked) return to(IDLE, 'fee-cap', { feeDay: day, steadySince: null });

  if (!input.refOk) return to(IDLE, 'reference', { steadySince: null });
  if (input.jump > cfg.killJump) return to(IDLE, 'jump', { steadySince: null });
  if (input.pegOk === false) return to(IDLE, 'peg', { steadySince: null });

  if (prev.mode === QUOTING) return to(QUOTING, null, { steadySince: prev.steadySince ?? nowMs });

  // idle with the reason cleared: wait until the reference has been fresh and steady for the cool-down
  const steadySince = prev.reason === 'cooldown' && prev.steadySince != null ? prev.steadySince : nowMs;
  if (nowMs - steadySince >= cfg.resumeAfterSec * 1000) return to(QUOTING, null, { steadySince });
  return to(IDLE, 'cooldown', { steadySince });
}

/** Whether a side quotes, and why not. */
export function sideStatus(levels, budget) {
  if (levels.length) return { quoting: true, reason: null };
  return { quoting: false, reason: budget > 0 ? 'inventory below one level' : 'no inventory' };
}

/** The retry delay after `failures` failed cycles: doubling from baseMs, capped at maxMs. */
export function backoffMs(failures, baseMs, maxMs) {
  if (failures <= 0) return 0;
  return Math.min(maxMs, baseMs * 2 ** Math.min(failures - 1, 30));
}

/** Amends and placements stop at the cap minus cancelReserveKas; cancels always go. */
export function feeGate(spentSompi, cfg) {
  const cap = Math.floor(cfg.maxFeesKasPerDay * 1e8);
  const tradeCap = cap - Math.floor(cfg.cancelReserveKas * 1e8);
  return { canTrade: spentSompi < tradeCap, canCancel: true, left: Math.max(0, tradeCap - spentSompi), overCap: spentSompi >= cap };
}
