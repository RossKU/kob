// Market history across a re-issue (no `@/` imports: unit-tested with plain `node --test`).
//
// A token issued again (the switch of the soak tokens to KOB's standard 3 / 3 program, 2026-10-10) gets a new covenant id, and the
// indexer keys every market read by covenant id: the trades, candles, 24 h stats and last price of `/v1/trades|candles|stats/{token}`
// come from the `fill` rows of `order_events` (`token_cov_id`), the pair pages from `pair_fills` (`base_cov_id`, `quote_cov_id`) and
// the KAS candles of the two legs. `carryHistory` moves those rows of the old id to the new id of the same TICKER, so the market page
// of the new token shows one continuous chart. Nothing else is touched: the old orders keep their own `token_cov_id` (their token
// template is not the new one, and `/v1/tokens` lists tokens from the orders), the old token's UTXO tables stay as they are.
//
// The executor must be stopped (the database is its derived view; the lock is not checked here). `kob-executor index replay` rebuilds
// the tables from the record log and undoes the carry: run it again after a replay (it is idempotent). Each carried id is recorded in
// `meta` (`carried_history:<old id>`), so a second run reports `already` and an attempt to carry the same id elsewhere is refused.

export interface RegistryLike {
  tokens: Array<{ ticker: string; covenant_id: string }>;
}

/** One re-issued ticker: its old and new covenant id (lower-case hex). */
export interface CarryPair {
  ticker: string;
  from: string;
  to: string;
}

/** The subset of node:sqlite's `DatabaseSync` the carry needs. */
export interface SqlDb {
  exec(sql: string): void;
  prepare(sql: string): {
    run(...params: unknown[]): { changes: number | bigint };
    get(...params: unknown[]): unknown;
  };
}

export interface CarryResult extends CarryPair {
  /** `fill` events moved to the new id (KAS-book trades and the fill events of pair orders whose token A it was) */
  fills: number;
  /** pair fills whose base token A / quote token B it was */
  pairBase: number;
  pairQuote: number;
  /** the meta record was already there (a re-run: nothing of the old id was left, or only rows added after the first run) */
  already: boolean;
}

const HEX32 = /^[0-9a-f]{64}$/;

/** The re-issued tickers between two registries: every ticker of `before` whose covenant id changed in `after`. */
export function carryPairs(before: RegistryLike, after: RegistryLike): CarryPair[] {
  const index = (r: RegistryLike, what: string): Map<string, string> => {
    const m = new Map<string, string>();
    for (const t of r.tokens ?? []) {
      const id = String(t.covenant_id).toLowerCase();
      if (!HEX32.test(id)) throw new Error(`${what} registry: ${t.ticker} has no 32-byte covenant id (${t.covenant_id})`);
      if (m.has(t.ticker)) throw new Error(`${what} registry: ticker ${t.ticker} appears twice`);
      m.set(t.ticker, id);
    }
    return m;
  };
  const a = index(before, 'old');
  const b = index(after, 'new');
  const out: CarryPair[] = [];
  const oldIds = new Set(a.values());
  for (const [ticker, from] of a) {
    const to = b.get(ticker);
    if (!to) throw new Error(`ticker ${ticker} of the old registry is not in the new one: its history would have no market to go to`);
    if (to === from) continue;
    // ids only move to fresh ones: a new id that is another ticker's old id would merge two histories
    if (oldIds.has(to)) throw new Error(`the new id of ${ticker} (${to}) is an old id of another token`);
    out.push({ ticker, from, to });
  }
  return out;
}

const hex = (s: string): Uint8Array => Uint8Array.from(Buffer.from(s, 'hex'));
const num = (n: number | bigint): number => Number(n);

/**
 * Moves the market history of every pair (old id -> new id) in ONE transaction. Refuses (and changes nothing) when an order of an old id
 * is still open or partial (its later fills would stay under the old id: end every order first), when an old id was carried to another
 * id before, or when the database lacks the indexer tables.
 */
export function carryHistory(db: SqlDb, pairs: CarryPair[], opts: { allowLive?: boolean; now?: () => number } = {}): CarryResult[] {
  for (const t of ['order_events', 'pair_fills', 'orders', 'order_state', 'meta']) {
    if (!db.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?").get(t)) throw new Error(`not a KOB indexer database: no table ${t}`);
  }
  const out: CarryResult[] = [];
  db.exec('BEGIN IMMEDIATE');
  try {
    for (const p of pairs) {
      if (!HEX32.test(p.from) || !HEX32.test(p.to) || p.from === p.to) throw new Error(`bad pair ${p.ticker}: ${p.from} -> ${p.to}`);
      const live = db
        .prepare(
          "SELECT COUNT(*) AS n FROM orders o JOIN order_state s ON s.covenant_id = o.covenant_id \
           WHERE (o.token_cov_id = ?1 OR o.quote_cov_id = ?1) AND s.status IN ('open', 'partial')",
        )
        .get(hex(p.from)) as { n: number | bigint };
      if (num(live.n) > 0 && !opts.allowLive) {
        throw new Error(`${p.ticker} (${p.from}) still has ${num(live.n)} open / partial orders: end them before the history moves`);
      }
      const key = `carried_history:${p.from}`;
      const prev = db.prepare('SELECT v FROM meta WHERE k = ?').get(key) as { v: string } | undefined;
      if (prev) {
        const was = JSON.parse(prev.v) as { to?: string };
        if (was.to !== p.to) throw new Error(`${p.ticker}: ${p.from} was carried to ${was.to} before, not to ${p.to}`);
      }
      const fills = num(db.prepare("UPDATE order_events SET token_cov_id = ?2 WHERE kind = 'fill' AND token_cov_id = ?1").run(hex(p.from), hex(p.to)).changes);
      const pairBase = num(db.prepare('UPDATE pair_fills SET base_cov_id = ?2 WHERE base_cov_id = ?1').run(hex(p.from), hex(p.to)).changes);
      const pairQuote = num(db.prepare('UPDATE pair_fills SET quote_cov_id = ?2 WHERE quote_cov_id = ?1').run(hex(p.from), hex(p.to)).changes);
      const rec = prev ? { ...JSON.parse(prev.v), rerun: (opts.now ?? Date.now)() } : { ticker: p.ticker, to: p.to, at: (opts.now ?? Date.now)(), fills, pairBase, pairQuote };
      db.prepare('INSERT OR REPLACE INTO meta (k, v) VALUES (?, ?)').run(key, JSON.stringify(rec));
      out.push({ ...p, fills, pairBase, pairQuote, already: !!prev });
    }
    db.exec('COMMIT');
  } catch (e) {
    db.exec('ROLLBACK');
    throw e;
  }
  return out;
}
