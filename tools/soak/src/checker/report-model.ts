// Pure part of the soak summary report: aggregation of the collected inputs and the compact text form (no I/O, no `@/` imports).
import type { Incident } from './incidents.ts';

export interface OrderRow {
  contract: string;
  tif: number | null;
  status: string;
  side: number;
}

export interface ActionRow {
  action: string;
  tries: number;
  placed: number;
  refused: number;
  refusals: [string, number][];
  txOk: number;
  conflicts: number;
  failed: number;
  errors: number;
}

export interface ProcSample {
  pid?: number;
  wsMb: number;
  privMb?: number;
  cpuSec?: number;
}
export interface ResourceSample {
  ts: number;
  procs: Record<string, ProcSample>;
  totalWsMb: number;
}

export interface ExecutorRow {
  name: string;
  metrics: Record<string, number> | null;
  health: { state: string; cursor_daa: number; lag_daa: number | null; orders_total?: number; fills_total?: number } | null;
}

/** one soak token's market (its fills from the checker, open orders from the indexer) */
export interface MarketRow {
  ticker: string;
  covenantId: string;
  decimals: number;
  fills: { count: number; sell: number; buy: number; trades: number; volumeTokens: string; volumeKas: string };
  openAsks: number | null;
  openBids: number | null;
  /** supply check: live - supply, the accepted baseline (null: not established yet) */
  supply?: { delta: string; baseline: string | null } | null;
}

export interface ReportData {
  now: number;
  soakStartMs: number | null;
  token: { ticker: string; covenantId: string; decimals: number };
  /** every soak token (primary first); absent in reports of a single-token soak */
  markets?: MarketRow[];
  /** transactions touching several books / with pair order fills (checker) */
  multi?: { txMultiBook: number; txPair?: number; pairFills?: number; txMultiPair?: number } | null;
  fills: { count: number; sell: number; buy: number; trades: number; volumeTokens: string; volumeKas: string; asOf: number | null };
  openAsks: number | null;
  openBids: number | null;
  orders: OrderRow[];
  ordersComplete: boolean;
  bots: { counters: Record<string, number>; gauges: Record<string, unknown>; ts: number | null } | null;
  executors: ExecutorRow[];
  x402: { paidByPath: Record<string, number>; errors: Record<string, number>; ledger: Record<string, unknown> | null };
  incidents: Incident[];
  checks: Record<string, number>;
  checkerTs: number | null;
  resources: { current: ResourceSample | null; peak: Record<string, number>; peakTotal: number };
  supervisor: { name: string; running: boolean; restarts: number }[];
}

// ------------------------------------------------------------------------------------------------ formatting helpers

export const fmtKas = (sompi: bigint | string | number | null | undefined, dp = 4): string => {
  if (sompi === null || sompi === undefined) return '?';
  const v = BigInt(typeof sompi === 'number' ? Math.round(sompi) : sompi);
  const neg = v < 0n;
  const a = neg ? -v : v;
  const whole = a / 100_000_000n;
  const frac = (a % 100_000_000n).toString().padStart(8, '0').slice(0, dp);
  return `${neg ? '-' : ''}${whole}${dp ? '.' + frac : ''}`;
};

export const fmtUnits = (base: bigint | string, decimals: number, dp = 2): string => {
  const v = BigInt(base);
  const d = 10n ** BigInt(decimals);
  const whole = v / d;
  const frac = (v % d).toString().padStart(decimals, '0').slice(0, dp);
  return dp ? `${whole}.${frac}` : `${whole}`;
};

export const fmtDur = (ms: number): string => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  return `${d ? d + 'd ' : ''}${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
};

const pad = (s: string | number, n: number) => String(s).padEnd(n);
const top = (m: Record<string, number>, n: number): [string, number][] => Object.entries(m).sort((a, b) => b[1] - a[1]).slice(0, n);

// ------------------------------------------------------------------------------------------------ aggregation

/** bot counters (`try:<a>`, `placed:<a>`, `plan_refused:<a>:<code>`, `tx_ok:<a>`, `tx_conflict:<a>`, `tx_failed:<a>`, `plan_throw:<a>`, `act_error:<a>`) per action */
export function botActions(counters: Record<string, number>): ActionRow[] {
  const rows = new Map<string, ActionRow>();
  const row = (a: string) => {
    let r = rows.get(a);
    if (!r) rows.set(a, (r = { action: a, tries: 0, placed: 0, refused: 0, refusals: [], txOk: 0, conflicts: 0, failed: 0, errors: 0 }));
    return r;
  };
  const refusals = new Map<string, Record<string, number>>();
  for (const [k, v] of Object.entries(counters)) {
    const i = k.indexOf(':');
    if (i < 0) continue;
    const kind = k.slice(0, i);
    const rest = k.slice(i + 1);
    switch (kind) {
      case 'try':
        row(rest).tries += v;
        break;
      case 'placed':
        row(rest).placed += v;
        break;
      case 'plan_refused': {
        const j = rest.lastIndexOf(':');
        const a = j < 0 ? rest : rest.slice(0, j);
        const code = j < 0 ? '?' : rest.slice(j + 1);
        row(a).refused += v;
        const m = refusals.get(a) ?? {};
        m[code] = (m[code] ?? 0) + v;
        refusals.set(a, m);
        break;
      }
      case 'tx_ok':
        row(rest).txOk += v;
        break;
      case 'tx_conflict':
        row(rest).conflicts += v;
        break;
      case 'tx_failed':
        row(rest).failed += v;
        break;
      case 'plan_throw':
      case 'act_error':
        row(rest).errors += v;
        break;
      default:
    }
  }
  for (const [a, m] of refusals) row(a).refusals = top(m, 3);
  return [...rows.values()].sort((a, b) => b.tries + b.txOk - (a.tries + a.txOk) || a.action.localeCompare(b.action));
}

export function ordersSummary(rows: OrderRow[]): { total: number; active: number; byContract: Record<string, { total: number; byStatus: Record<string, number>; byTif: Record<string, number> }> } {
  const byContract: Record<string, { total: number; byStatus: Record<string, number>; byTif: Record<string, number> }> = {};
  let active = 0;
  for (const o of rows) {
    const c = (byContract[o.contract] ??= { total: 0, byStatus: {}, byTif: {} });
    c.total++;
    c.byStatus[o.status] = (c.byStatus[o.status] ?? 0) + 1;
    const tif = o.tif === null || o.tif === undefined ? '-' : String(o.tif);
    c.byTif[tif] = (c.byTif[tif] ?? 0) + 1;
    if (o.status === 'open' || o.status === 'partial') active++;
  }
  return { total: rows.length, active, byContract };
}

/** peak working set per process (and of the total) over the samples at or after `sinceMs` (resources.jsonl text) */
export function resourcePeaks(jsonl: string, sinceMs: number): { peak: Record<string, number>; peakTotal: number } {
  const peak: Record<string, number> = {};
  let peakTotal = 0;
  for (const line of jsonl.split('\n')) {
    if (!line.trim()) continue;
    let s: ResourceSample;
    try {
      s = JSON.parse(line) as ResourceSample;
    } catch {
      continue;
    }
    if (s.ts < sinceMs) continue;
    for (const [n, p] of Object.entries(s.procs ?? {})) peak[n] = Math.max(peak[n] ?? 0, p.wsMb ?? 0);
    peakTotal = Math.max(peakTotal, s.totalWsMb ?? 0);
  }
  return { peak, peakTotal };
}

export function incidentSummary(incs: Incident[]): { total: number; errors: number; byInvariant: Record<string, { error: number; warn: number }> } {
  const byInvariant: Record<string, { error: number; warn: number }> = {};
  let errors = 0;
  for (const i of incs) {
    const b = (byInvariant[i.invariant] ??= { error: 0, warn: 0 });
    if (i.severity === 'error') {
      b.error++;
      errors++;
    } else b.warn++;
  }
  return { total: incs.length, errors, byInvariant };
}

const TIF_NAME: Record<string, string> = { '0': 'gtc', '1': 'ioc', '2': 'fok', '-': '' };
const INVARIANTS = ['custody', 'stray', 'all-in', 'ioc-fok', 'trigger', 'repeat', 'x402', 'fee', 'indexers-agree', 'health', 'pair', 'supply'];

// ------------------------------------------------------------------------------------------------ JSON + text

export function buildReport(d: ReportData): { json: Record<string, unknown>; text: string } {
  const orders = ordersSummary(d.orders);
  const actions = d.bots ? botActions(d.bots.counters) : [];
  const incs = incidentSummary(d.incidents);
  const c = d.bots?.counters ?? {};
  const conflictsBots = c.tx_conflict ?? 0;
  const conflictsMatchers = d.executors.reduce((s, e) => s + (e.metrics?.kob_matcher_conflicts_total ?? 0), 0);
  const L: string[] = [];
  const iso = new Date(d.now).toISOString().replace('T', ' ').slice(0, 16);
  L.push(`KOB TN10 soak  ${iso} UTC  uptime ${d.soakStartMs ? fmtDur(d.now - d.soakStartMs) : '?'}  token ${d.token.ticker} ${d.token.covenantId.slice(0, 8)}`);
  L.push(
    `indexers ${d.executors.map((e) => `${e.name} ${e.health ? `${e.health.state} lag ${e.health.lag_daa ?? '?'} cursor ${e.health.cursor_daa}` : 'unreachable'}`).join(' | ')}`,
  );
  const f = d.fills;
  if (d.markets && d.markets.length > 1) {
    for (const m of d.markets) {
      const g = m.fills;
      L.push(
        `market   ${pad(m.ticker, 5)} fills ${g.count} (sell ${g.sell} / buy ${g.buy})  trades ${g.trades}  volume ${fmtUnits(g.volumeTokens, m.decimals, m.ticker === d.token.ticker ? 2 : 4)} ${m.ticker} / ${fmtKas(g.volumeKas, 2)} KAS  open asks ${m.openAsks ?? '?'} bids ${m.openBids ?? '?'}`,
      );
    }
    const x = d.multi;
    if (x) L.push(`multi    txs filling 2+ books ${x.txMultiBook}  with a pair fill ${x.txPair ?? 0} (2+ pair fills ${x.txMultiPair ?? 0})  pair fills ${x.pairFills ?? 0}  all txs ${f.trades}`);
    const sup = d.markets.filter((m) => m.supply).map((m) => `${m.ticker} delta ${m.supply!.delta}${m.supply!.baseline !== null && m.supply!.baseline !== '0' ? ` (baseline ${m.supply!.baseline}: pre-run holdings no tracker knows)` : m.supply!.baseline === null ? ' (baseline pending)' : ''}`);
    if (sup.length) L.push(`supply   ${sup.join('  ')}  [live token UTXOs - supply, base units]`);
  } else {
    L.push(
      `market   fills ${f.count} (sell ${f.sell} / buy ${f.buy})  trades ${f.trades}  volume ${fmtUnits(f.volumeTokens, d.token.decimals)} ${d.token.ticker} / ${fmtKas(f.volumeKas, 2)} KAS  open asks ${d.openAsks ?? '?'} bids ${d.openBids ?? '?'}`,
    );
  }
  L.push(`orders   total ${orders.total}${d.ordersComplete ? '' : '+'}  active ${orders.active}`);
  for (const [k, v] of Object.entries(orders.byContract).sort((a, b) => b[1].total - a[1].total)) {
    const tif = Object.entries(v.byTif).filter(([t]) => t !== '-').map(([t, n]) => `${TIF_NAME[t] ?? 'tif' + t} ${n}`).join(' ');
    const st = Object.entries(v.byStatus).sort((a, b) => b[1] - a[1]).map(([s, n]) => `${s} ${n}`).join(' ');
    L.push(`  ${pad(k, 11)} ${pad(v.total, 5)} ${st}${tif ? `  [${tif}]` : ''}`);
  }
  L.push(`bots     txs ok ${c.tx_ok ?? 0} conflict ${conflictsBots} failed ${c.tx_failed ?? 0}  fees ${fmtKas(c.fees_sompi ?? 0)} KAS${d.bots?.ts ? `  (as of ${fmtDur(d.now - d.bots.ts)} ago)` : ''}`);
  L.push(`  ${pad('action', 24)} ${pad('try', 5)} ${pad('ok', 5)} ${pad('refused', 8)} ${pad('confl', 6)} ${pad('fail', 5)} top refusals`);
  for (const a of actions) {
    if (!a.tries && !a.txOk && !a.refused) continue;
    L.push(
      `  ${pad(a.action, 24)} ${pad(a.tries || '-', 5)} ${pad(a.placed || a.txOk, 5)} ${pad(a.refused, 8)} ${pad(a.conflicts, 6)} ${pad(a.failed + a.errors, 5)} ${a.refusals.map(([k, n]) => `${k} ${n}`).join(', ')}`,
    );
  }
  L.push('executors');
  for (const e of d.executors) {
    const m = e.metrics;
    if (!m) {
      L.push(`  ${e.name}  no metrics`);
      continue;
    }
    L.push(
      `  ${pad(e.name, 7)} finalized ${m.kob_matcher_finalized_total ?? 0} (profit ${fmtKas(m.kob_matcher_finalized_profit_sompi ?? 0)} KAS)  submitted ${m.kob_matcher_submitted_total ?? 0}  conflicts ${m.kob_matcher_conflicts_total ?? 0}  rejected ${m.kob_matcher_rejected_total ?? 0}  pending ${m.kob_matcher_pending_txs ?? 0}  funding ${fmtKas(m.kob_matcher_funding_sompi ?? 0, 2)} KAS${m.kob_operator_low_funds ? ' LOW' : ''}${m.kob_matcher_node_synced === 0 ? ' UNSYNCED' : ''}${m.kob_matcher_book_stale ? ' BOOK-STALE' : ''}`,
    );
  }
  L.push(`  races (RBF / double-spend between executors and bots): matcher conflicts ${conflictsMatchers} + bot tx conflicts ${conflictsBots}`);
  const paid = Object.entries(d.x402.paidByPath).map(([p, n]) => `${p} ${n}`).join(' ') || '0';
  const errs = top(d.x402.errors, 4).map(([k, n]) => `${k} ${n}`).join(', ');
  const lg = d.x402.ledger as { ledgerAccepted?: number; ledgerFailed?: number; ledgerAmbiguous?: number; byKind?: Record<string, number> } | null;
  L.push(
    `x402     paid ${paid}${errs ? `  errors ${errs}` : ''}${lg ? `  ledger accepted ${lg.ledgerAccepted ?? 0} (${Object.entries(lg.byKind ?? {}).map(([k, n]) => `${k} ${n}`).join(' ')}) failed ${lg.ledgerFailed ?? 0} ambiguous ${lg.ledgerAmbiguous ?? 0}` : ''}`,
  );
  const k = d.checks;
  L.push(
    `checks   fills ${k.fills ?? 0} (bound ${k.fillBound ?? 0}, payout ${k.fillPayout ?? 0}${k.fillsUnverifiable ? `, no terms ${k.fillsUnverifiable}` : ""})  custody ${k.custody ?? 0}  node utxos ${k.nodeOutpoints ?? 0}  strays ${k.straysLive ?? 0} live / ${k.straysSwept ?? 0} swept  ioc/fok ${k.killChecked ?? 0}  arm/trail ${k.triggers ?? 0} (ok ${k.triggersOk ?? 0}${k.triggersUnverifiable ? `, unverifiable ${k.triggersUnverifiable}` : ''})  rearm ${k.rearms ?? 0}  repeat ${k.repeatEntries ?? 0}`,
  );
  L.push(
    `         pair ${k.pairChecked ?? 0}/${k.pairFills ?? 0} (route ${k['pairCounterparty:route'] ?? 0}, netting ${k['pairCounterparty:netting'] ?? 0}, inventory ${k['pairCounterparty:inventory'] ?? 0}; triggers ${k.pairTriggers ?? 0}: mode 0 ${k.pairTriggerMode0 ?? 0}, mode 1 ${k.pairTriggerMode1 ?? 0})  supply ${k.supplyOk ?? 0}/${k.supplyChecks ?? 0}  x402 ${k.x402Settled ?? 0}/${k.x402Payments ?? 0}  fee lines ${k.feeLines ?? 0}  agree orders ${k.agreeOrders ?? 0} windows ${k.agreeWindows ?? 0}${d.checkerTs ? `  (checker ${fmtDur(d.now - d.checkerTs)} ago, ${k.rounds ?? 0} rounds)` : '  (checker not run)'}`,
  );
  L.push(
    `incidents ${incs.total} (errors ${incs.errors})  ${INVARIANTS.map((i) => `${i} ${(incs.byInvariant[i]?.error ?? 0) + (incs.byInvariant[i]?.warn ?? 0)}`).join('  ')}`,
  );
  for (const i of d.incidents.slice(-5)) {
    const p = (i.detail as { problem?: string }).problem ?? '';
    L.push(`  ${i.ts.slice(5, 16)} ${i.severity} ${i.invariant} ${i.subject.slice(0, 40)} ${p}`);
  }
  const cur = d.resources.current;
  if (cur) {
    const names = Object.keys(cur.procs);
    L.push(
      `memory   total ${cur.totalWsMb} MB (peak ${d.resources.peakTotal})  ${names.map((n) => `${n} ${cur.procs[n].wsMb}/${d.resources.peak[n] ?? cur.procs[n].wsMb}`).join('  ')}  [MB now/peak]`,
    );
    L.push(`cpu      ${names.map((n) => `${n} ${cur.procs[n].cpuSec ?? '?'}s`).join('  ')}`);
  } else L.push('memory   no samples');
  L.push(`restarts ${d.supervisor.map((s) => `${s.name} ${s.restarts}${s.running ? '' : ' (down)'}`).join('  ') || '?'}`);

  const json = {
    ts: new Date(d.now).toISOString(),
    uptimeSec: d.soakStartMs ? Math.round((d.now - d.soakStartMs) / 1000) : null,
    token: d.token,
    fills: d.fills,
    markets: d.markets ?? null,
    multi: d.multi ?? null,
    openAsks: d.openAsks,
    openBids: d.openBids,
    orders: { ...orders, complete: d.ordersComplete },
    bots: { actions, txOk: c.tx_ok ?? 0, txConflict: conflictsBots, txFailed: c.tx_failed ?? 0, feesSompi: c.fees_sompi ?? 0, gauges: d.bots?.gauges ?? {}, ts: d.bots?.ts ?? null },
    executors: d.executors,
    races: { matcherConflicts: conflictsMatchers, botConflicts: conflictsBots },
    x402: d.x402,
    checks: d.checks,
    checkerTs: d.checkerTs,
    incidents: { ...incs, last: d.incidents.slice(-20) },
    resources: d.resources,
    supervisor: d.supervisor,
  };
  return { json, text: L.join('\n') };
}
