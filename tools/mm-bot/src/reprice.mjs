// The reprice decision: amend in place past the threshold; cancel only where an amend cannot work. Never on a timer.

import { bidCost, orderValue } from './ladder.mjs';

/** The cycle's actions: cancels, then amends, then placements (only what the free inventory covers now). */
export function planActions(targets, orders, ctx) {
  const { fair, scale, q } = ctx;
  const minValue = Math.floor(q.minOrderKas * 1e8);
  const carrierMin = Math.floor(q.askCarrierMinKas * 1e8);
  const cancels = [];
  const amends = [];
  const places = [];
  let freeKas = ctx.free.kas;
  let freeTokens = ctx.free.tokens;

  for (const [side, levels] of [['buy', targets.bids], ['sell', targets.asks]]) {
    const mine = orders.filter((o) => o.side === side).sort((a, b) => a.level - b.level || a.id.localeCompare(b.id));
    const byLevel = new Map();
    for (const o of mine) {
      const target = levels.find((t) => t.level === o.level);
      if (!target) {
        if (!o.pending) cancels.push({ kind: 'cancel', id: o.id, side, level: o.level, reason: 'level no longer quoted' });
        continue;
      }
      if (byLevel.has(o.level)) {
        if (!o.pending) cancels.push({ kind: 'cancel', id: o.id, side, level: o.level, reason: 'second order at one level' });
        continue;
      }
      byLevel.set(o.level, o);
    }

    for (const t of levels) {
      const o = byLevel.get(t.level);
      if (!o) {
        const fits = side === 'buy' ? bidCost(t.amount, t.price, scale) <= freeKas : t.amount <= freeTokens;
        if (!fits) continue;
        if (side === 'buy') freeKas -= bidCost(t.amount, t.price, scale);
        else freeTokens -= t.amount;
        places.push({ kind: 'place', side, level: t.level, price: t.price, amount: t.amount, quotedFair: fair, reason: 'level empty' });
        continue;
      }
      if (o.pending) continue;
      const small = o.amountLeft < q.resizeBelow * t.amount || orderValue(o.amountLeft, o.price, scale) < minValue;
      if (small && t.amount > o.amountLeft) {
        cancels.push({ kind: 'cancel', id: o.id, side, level: t.level, reason: 'order too small for its level; to be placed again' });
        continue;
      }
      // `off` follows a config or skew change on the same threshold
      const moved = Math.abs(fair / o.quotedFair - 1);
      const off = Math.abs(t.price / o.price - 1);
      if (Math.max(moved, off) <= q.repriceThreshold || t.price === o.price) continue;
      if (side === 'sell' && o.value < carrierMin) {
        cancels.push({ kind: 'cancel', id: o.id, side, level: t.level, reason: 'carrier too low to amend; to be placed again' });
        continue;
      }
      if (side === 'buy' && orderValue(Math.floor((o.value * scale) / t.price), t.price, scale) < minValue) {
        cancels.push({ kind: 'cancel', id: o.id, side, level: t.level, reason: 'escrow too small at the new price' });
        continue;
      }
      amends.push({ kind: 'amend', id: o.id, side, level: t.level, from: o.price, price: t.price, quotedFair: fair,
        reason: moved > q.repriceThreshold ? `fair moved ${(moved * 100).toFixed(3)}% since quoted` : `${(off * 100).toFixed(3)}% from its target` });
    }
  }
  return [...cancels, ...amends, ...places];
}

/** A cancel for every quote. */
export function pullAll(orders, reason) {
  return orders.map((o) => ({ kind: 'cancel', id: o.id, side: o.side, level: o.level, reason }));
}
