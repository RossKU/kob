// The soak summary report: run/reports/latest.{json,txt}, plus a timestamped copy per hour (report-YYYYMMDD-HH.{json,txt}). Inputs:
// both indexers (health, the full order list of the token, open asks / bids), the executors' metrics files, the bots' counters, the
// checker's state (fills / volume / check counters), the incident log, the supervisor's status and resource samples. The text form is
// compact (scripts/status.ps1 prints it; it is pasted into status updates).
import { existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { HttpIndexer } from '@/data/indexer';
import type { Env } from '../env';
import { errText, logger } from '../log';
import { readIncidents } from './incidents.ts';
import { buildReport, resourcePeaks, type ExecutorRow, type OrderRow, type ReportData, type ResourceSample } from './report-model.ts';
import { parseProm } from './rules.ts';
import type { CheckerState } from './store.ts';

const log = logger('report');

function readJson<T>(p: string): T | null {
  try {
    return existsSync(p) ? (JSON.parse(readFileSync(p, 'utf8')) as T) : null;
  } catch {
    return null;
  }
}

function atomicWrite(p: string, s: string): void {
  writeFileSync(p + '.tmp', s);
  renameSync(p + '.tmp', p);
}

export async function writeReport(env: Env): Promise<{ text: string }> {
  const cfg = env.cfg;
  const run = cfg.runPath;
  const now = Date.now();
  const t = env.state.token;
  const token = t?.covenantId ?? '';

  // indexers + executors
  const executors: ExecutorRow[] = [];
  for (const e of cfg.executors) {
    const ix = new HttpIndexer({ baseUrl: `http://${e.api}`, timeoutMs: 10_000 });
    let health: ExecutorRow['health'] = null;
    try {
      const h = (await ix.health()) as unknown as { state: string; cursor_daa: number; lag_daa: number | null; counters?: { orders_total?: number; fills_total?: number } };
      health = { state: h.state, cursor_daa: h.cursor_daa, lag_daa: h.lag_daa, orders_total: h.counters?.orders_total, fills_total: h.counters?.fills_total };
    } catch {
      /* unreachable */
    }
    const mp = join(run, e.name, 'metrics.prom');
    executors.push({ name: e.name, health, metrics: existsSync(mp) ? parseProm(readFileSync(mp, 'utf8')) : null });
  }

  // orders of every soak token (paged; a pair order is listed under both its tokens: counted once) and the book counts
  const soakTokens = [t, env.state.token2, env.state.token3].filter((x): x is NonNullable<typeof t> => !!x);
  const orders: OrderRow[] = [];
  let ordersComplete = true;
  let openAsks: number | null = null;
  let openBids: number | null = null;
  const open = new Map<string, { asks: number | null; bids: number | null }>();
  if (token) {
    try {
      const maxPages = 250;
      const seen = new Set<string>();
      for (const st of soakTokens) {
        const all = await env.indexer.allOrders({ token: st.covenantId, limit: 200 }, { maxPages });
        if (all.length >= maxPages * 200) ordersComplete = false;
        for (const o of all) {
          if (seen.has(o.covenant_id)) continue;
          seen.add(o.covenant_id);
          orders.push({ contract: o.contract, tif: o.tif ?? null, status: o.status, side: o.side });
        }
      }
    } catch (e) {
      ordersComplete = false;
      log.warn('order listing failed', { error: errText(e) });
    }
    try {
      const tks = await env.indexer.tokens();
      for (const st of soakTokens) {
        const tk = tks.find((x) => x.covenant_id === st.covenantId);
        open.set(st.covenantId, { asks: tk?.open_asks ?? null, bids: tk?.open_bids ?? null });
      }
      openAsks = open.get(token)?.asks ?? null;
      openBids = open.get(token)?.bids ?? null;
    } catch {
      /* ignore */
    }
  }

  // bots, checker, incidents
  const bots = readJson<{ ts: number; counters: Record<string, number>; gauges: Record<string, unknown> }>(join(run, 'stats', 'bots.json'));
  const chk = readJson<CheckerState>(join(run, 'state', 'checker.json'));
  const incidents = readIncidents(join(run, 'incidents.jsonl'));
  const paidByPath: Record<string, number> = {};
  const errors: Record<string, number> = {};
  for (const [k, v] of Object.entries(bots?.counters ?? {})) {
    if (k.startsWith('x402_paid:')) paidByPath[k.slice(10)] = v;
    else if (k.startsWith('x402_error:') || k.startsWith('x402_unpaid:')) errors[k.replace(/^x402_(error|unpaid):/, (_m, g: string) => (g === 'unpaid' ? 'http ' : ''))] = v;
  }

  // supervisor + resources
  const sup = readJson<{ children: { name: string; running: boolean; restarts: number }[] }>(join(run, 'supervisor.json'));
  const pidFile = join(run, 'supervisor.pid');
  const soakStartMs = existsSync(pidFile) ? statSync(pidFile).mtimeMs : null;
  const current = readJson<ResourceSample>(join(run, 'stats', 'resources.json'));
  const rj = join(run, 'stats', 'resources.jsonl');
  const peaks = existsSync(rj) ? resourcePeaks(readFileSync(rj, 'utf8'), soakStartMs ?? 0) : { peak: {}, peakTotal: 0 };

  const f = chk?.fills;
  const data: ReportData = {
    now,
    soakStartMs,
    token: { ticker: t?.ticker ?? '?', covenantId: token, decimals: t?.decimals ?? 8 },
    fills: { count: f?.count ?? 0, sell: f?.sell ?? 0, buy: f?.buy ?? 0, trades: f?.trades ?? 0, volumeTokens: f?.volumeTokens ?? '0', volumeKas: f?.volumeKas ?? '0', asOf: chk?.updatedAt ?? null },
    markets: soakTokens.map((st) => {
      const bt = f?.byToken?.[st.covenantId];
      const sp = chk?.supply?.[st.covenantId];
      return {
        ticker: st.ticker,
        covenantId: st.covenantId,
        decimals: st.decimals,
        fills: bt ?? { count: 0, sell: 0, buy: 0, trades: 0, volumeTokens: '0', volumeKas: '0' },
        openAsks: open.get(st.covenantId)?.asks ?? null,
        openBids: open.get(st.covenantId)?.bids ?? null,
        supply: sp ? { delta: sp.last, baseline: sp.baseline } : null,
      };
    }),
    multi: f?.multi ?? null,
    openAsks,
    openBids,
    orders,
    ordersComplete,
    bots: bots ? { counters: bots.counters ?? {}, gauges: bots.gauges ?? {}, ts: bots.ts ?? null } : null,
    executors,
    x402: { paidByPath, errors, ledger: chk?.x402 ?? null },
    incidents,
    checks: chk?.stats ?? {},
    checkerTs: chk?.updatedAt ?? null,
    resources: { current, peak: peaks.peak, peakTotal: Math.max(peaks.peakTotal, current?.totalWsMb ?? 0) },
    supervisor: (sup?.children ?? []).map((c) => ({ name: c.name, running: c.running, restarts: c.restarts })),
  };
  const { json, text } = buildReport(data);

  const dir = join(run, 'reports');
  mkdirSync(dir, { recursive: true });
  const body = JSON.stringify(json, (_k, v) => (typeof v === 'bigint' ? v.toString() : v), 2);
  atomicWrite(join(dir, 'latest.json'), body);
  atomicWrite(join(dir, 'latest.txt'), text + '\n');
  const stamp = new Date(now).toISOString().replace(/[-:]/g, '').replace('T', '-').slice(0, 11);
  const hourly = join(dir, `report-${stamp}.txt`);
  if (!existsSync(hourly)) {
    writeFileSync(hourly, text + '\n');
    writeFileSync(join(dir, `report-${stamp}.json`), body);
  }
  return { text };
}
