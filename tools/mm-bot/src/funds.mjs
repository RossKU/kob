// Funding checks and deposit messages, in % of the capital, USD and units.

import { levelOffset } from './ladder.mjs';
import { pctOf } from './sizing.mjs';

/** A number with thousands separators and up to `dp` decimals. */
export function fmtAmount(x, dp = 2) {
  return x.toLocaleString('en-US', { minimumFractionDigits: 0, maximumFractionDigits: dp });
}

/** What the full ladders need, in whole units: { bidKas, askTokens, reserveKas }. */
export function ladderNeeds(fair, q) {
  let bidKas = 0;
  for (let i = 0; i < q.levels; i++) bidKas += q.sizePerLevel * fair * (1 - levelOffset(q, i));
  return {
    bidKas: Math.min(q.maxBidKas, bidKas),
    askTokens: Math.min(q.maxAskTokens, q.levels * q.sizePerLevel),
    reserveKas: q.kasReserve,
  };
}

/** A shortfall: `needed - held`, never negative. */
export const shortfall = (held, needed) => Math.max(0, needed - held);

/** idle: the side cannot quote; warn: held under warnUsd; else ok. */
export function fundLevel(heldUsd, warnUsd, quoting) {
  if (quoting === false) return 'idle';
  if (heldUsd < warnUsd) return 'warn';
  return 'ok';
}

/** The bids, asks and fees checks, each with its deposit message (null when ok). */
export function fundChecks(x) {
  const need = ladderNeeds(x.fair, x.q);
  const cap = x.capitalUsd;
  const pct = (usd) => `${fmtAmount(pctOf(usd, cap), 1)}%`;
  const usd = (v) => `${fmtAmount(v, 0)} USD`;
  const warnUsd = (cap * x.warnBelowPct) / 100;
  const tokenName = x.token.label ? `${x.token.label} (${x.token.id})` : x.token.id;
  const tokenUnit = x.token.label ?? 'tokens';
  const out = [];

  const bidNeed = need.bidKas + need.reserveKas;
  const bidNeedUsd = need.bidKas * x.rates.kasUsd;
  const bidHeldUsd = x.kasHeld * x.rates.kasUsd;
  const bidLevel = fundLevel(bidHeldUsd, warnUsd, x.bidsQuoting);
  const bidShort = shortfall(x.kasHeld, bidNeed);
  out.push({
    key: 'bids', level: bidLevel, asset: 'KAS', held: x.kasHeld, needed: bidNeed, shortfall: bidShort, unit: 'KAS',
    message: bidLevel === 'ok' ? null
      : `${bidLevel === 'idle' ? 'Insufficient' : 'Low'} KAS: bids need ${pct(bidNeedUsd)} = ${usd(bidNeedUsd)} = ${fmtAmount(need.bidKas, 0)} KAS `
        + `plus a ${fmtAmount(need.reserveKas)} KAS reserve, ${fmtAmount(x.kasHeld)} KAS (${pct(bidHeldUsd)}) held; `
        + `deposit ${fmtAmount(bidShort)} KAS to ${x.address} ${bidLevel === 'idle' ? 'to resume bids' : 'to quote the full bid ladder'}.`,
  });

  const askNeedUsd = need.askTokens * x.rates.tokenUsd;
  const askHeldUsd = x.tokenHeld * x.rates.tokenUsd;
  const askLevel = fundLevel(askHeldUsd, warnUsd, x.asksQuoting);
  const askShort = shortfall(x.tokenHeld, need.askTokens);
  out.push({
    key: 'asks', level: askLevel, asset: x.token.id, held: x.tokenHeld, needed: need.askTokens, shortfall: askShort, unit: tokenUnit,
    message: askLevel === 'ok' ? null
      : `${askLevel === 'idle' ? 'Insufficient' : 'Low'} ${tokenName}: asks need ${pct(askNeedUsd)} = ${usd(askNeedUsd)} = ${fmtAmount(need.askTokens, 6)} ${tokenUnit}, `
        + `${fmtAmount(x.tokenHeld, 6)} ${tokenUnit} (${pct(askHeldUsd)}) held; `
        + `deposit ${fmtAmount(askShort, 6)} ${tokenUnit} to ${x.address} ${askLevel === 'idle' ? 'to resume asks' : 'to quote the full ask ladder'}.`,
  });

  const feeLevel = x.kasFree < need.reserveKas ? 'warn' : 'ok';
  const feeShort = shortfall(x.kasFree, need.reserveKas);
  out.push({
    key: 'fees', level: feeLevel, asset: 'KAS', held: x.kasFree, needed: need.reserveKas, shortfall: feeShort, unit: 'KAS',
    message: feeLevel === 'ok' ? null
      : `Low KAS for fees and carriers: ${fmtAmount(x.kasFree)} KAS free of a ${fmtAmount(need.reserveKas)} KAS reserve; `
        + `deposit ${fmtAmount(feeShort)} KAS to ${x.address}.`,
  });
  return out;
}

/**
 * Alerts due now (on a change, every remindMin while not ok, "funds received" on recovery); updates
 * `memory`.
 */
export function alertsToSend(checks, memory, nowMs, remindMin) {
  const out = [];
  for (const c of checks) {
    const last = memory[c.key];
    const changed = (last?.level ?? 'ok') !== c.level;
    const due = c.level !== 'ok' && last && nowMs - last.at >= remindMin * 60_000;
    if (!changed && !due) continue;
    memory[c.key] = { level: c.level, at: nowMs };
    if (c.level !== 'ok') {
      out.push(changed ? c : { ...c, message: `Reminder: ${c.message}` });
    } else if (last) {
      const what = c.key === 'bids' ? 'bid quoting resumed' : c.key === 'asks' ? 'ask quoting resumed' : 'the fee reserve is back';
      out.push({ ...c, level: 'info', message: last.level === 'idle' ? `Funds received, ${what}.` : `Funds received: ${c.key === 'fees' ? 'the fee reserve' : `the ${c.key === 'bids' ? 'bid' : 'ask'} ladder`} is funded again.` });
    }
  }
  return out;
}
