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

// ------------------------------------------------------------------------------------------------ chart check
//
// What every chart must show, checked after a redeploy (`scripts/chart-history.mjs`): the first candle of each interval is the
// bucket of the token's first KAS trade in the database, and no fill is left under an id carried to it (a replay of the record
// log undid the carry). A chart that starts later than the database lost history in the read path, not in the data: the
// 2026-10-14 case (the indexer read only the newest 20 000 fills of a window, so the TUSD chart began on 10-11 although its
// fills went back to 10-06).

/** The candle intervals of the API (`/v1/candles`, `/v1/pairs/{a}/{b}/candles`) and their length in ms. */
export const CHART_INTERVALS: ReadonlyArray<readonly [string, number]> = [
  ['1d', 86_400_000],
  ['1h', 3_600_000],
  ['5m', 300_000],
  ['1m', 60_000],
];

export interface HistoryFloor {
  ticker: string;
  id: string;
  /** ms of the first KAS-book fill under the current id (what `/v1/candles` reads), null without one */
  firstTrade: number | null;
  /** fills still under an old id carried to this one (`carried_history:<old id>` in `meta`): non-zero after a replay */
  leftover: number;
}

/** Per token of the registry: its first KAS trade and the fills left under its carried ids (read-only; the executor may run). */
export function historyFloors(db: SqlDb, reg: RegistryLike): HistoryFloor[] {
  const keys = (db.prepare("SELECT group_concat(k, ',') AS ks FROM meta WHERE k LIKE 'carried_history:%'").get() as { ks: string | null }).ks;
  const carriedTo = new Map<string, string[]>();
  for (const k of keys ? keys.split(',') : []) {
    const { v } = db.prepare('SELECT v FROM meta WHERE k = ?').get(k) as { v: string };
    const to = String((JSON.parse(v) as { to?: string }).to ?? '');
    carriedTo.set(to, [...(carriedTo.get(to) ?? []), k.slice('carried_history:'.length)]);
  }
  return (reg.tokens ?? []).map((t) => {
    const id = String(t.covenant_id).toLowerCase();
    // the same rows as the indexer's FILL_SELECT (market.rs): a priced KAS-book fill of a known order
    const first = db
      .prepare(
        "SELECT MIN(e.ts) AS t FROM order_events e WHERE e.kind = 'fill' AND e.token_cov_id = ? AND e.price IS NOT NULL AND e.amount > 0 \
         AND e.side IN (1, 2) AND EXISTS (SELECT 1 FROM orders o WHERE o.covenant_id = e.covenant_id)",
      )
      .get(hex(id)) as { t: number | bigint | null };
    let leftover = 0;
    for (const from of carriedTo.get(id) ?? []) {
      leftover += num((db.prepare("SELECT COUNT(*) AS n FROM order_events WHERE kind = 'fill' AND token_cov_id = ?").get(hex(from)) as { n: number | bigint }).n);
      leftover += num((db.prepare('SELECT COUNT(*) AS n FROM pair_fills WHERE base_cov_id = ?1 OR quote_cov_id = ?1').get(hex(from)) as { n: number | bigint }).n);
    }
    return { ticker: t.ticker, id, firstTrade: first.t == null ? null : num(first.t), leftover };
  });
}

/** The pair markets (base id, quote id) with pair fills between two tokens of the registry. */
export function pairMarkets(db: SqlDb, reg: RegistryLike): Array<[string, string]> {
  const ids = new Set((reg.tokens ?? []).map((t) => String(t.covenant_id).toLowerCase()));
  const r = db
    .prepare("SELECT group_concat(p, ',') AS ps FROM (SELECT DISTINCT lower(hex(base_cov_id)) || ':' || lower(hex(quote_cov_id)) AS p FROM pair_fills)")
    .get() as { ps: string | null };
  return (r.ps ? r.ps.split(',') : [])
    .map((p) => p.split(':') as [string, string])
    .filter(([a, b]) => ids.has(a) && ids.has(b))
    .sort();
}

/**
 * The problems of one market's charts: for each interval, its first candle (from the API, null for none) against the bucket of
 * `firstTrade` (a pair: the later of its two tokens' first trades, the first bucket in which both have a price).
 */
export function chartProblems(market: string, firstTrade: number | null, firstCandle: Record<string, number | null | undefined>): string[] {
  const out: string[] = [];
  const iso = (t: number | null | undefined): string => (t == null ? 'none' : new Date(t).toISOString().slice(0, 16) + 'Z');
  for (const [name, ms] of CHART_INTERVALS) {
    if (!(name in firstCandle)) continue;
    const want = firstTrade == null ? null : Math.floor(firstTrade / ms) * ms;
    const got = firstCandle[name] ?? null;
    if (got !== want) out.push(`${market} ${name}: the chart starts at ${iso(got)}, the first trade's bucket is ${iso(want)}`);
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ import from older runs
//
// Each soak run started its indexers on a fresh database, so the trades of the earlier runs (on the same chain, often of the same
// tokens) live only in those runs' databases. `importHistory` copies the KAS-book fills of one older database into the current
// one, by ticker onto the current covenant ids (as `carryHistory` does), so the charts reach back to the very first run:
//
// * the fills (`order_events` kind `fill` with a price) whose token maps to a current ticker, before `until` (the start of the
//   next run: where two runs overlap, the one live at the time wins), each once (an event of the same order and transaction is
//   never copied twice), with `block_seq` 0 (no chain block of this database: a reorg never reverts it);
// * the orders those fills belong to that the database does not hold (the market reads take the scale and the genesis DAA from
//   them): `genesis_block` 0, `maker` NULL (they never show in a maker's orders), `listed` 0 with `unlisted_reason`
//   `history_import:<label>`, their token id unchanged, and a final `order_state` (an order still open in the old run is
//   `closed`: it is no longer anyone's);
// * the pair fills of the mapped tokens (a database of protocol v3 or later).
//
// A database of the lot layouts (protocol v2, schema 4 or older: `orders.lot_units`) is converted exactly: a fill's amount is
// `lots * lot_units * unit` base units and its price stays sompi per `unit` base units, which is the imported order's `scale`.
// One transaction; the import is recorded in `meta` (`history_import:<label>`), so a re-run is a no-op. The executor must be
// stopped. `kob-executor index replay` rebuilds the tables from the record log and drops the import: import again after a replay.
// Undo: delete the `order_events` / `order_state` / `orders` rows of the orders whose `unlisted_reason` is the label's, and the meta row.

/** The read side of node:sqlite's `DatabaseSync` an import reads a source with. */
export interface SqlSrc {
  prepare(sql: string): {
    all(...params: unknown[]): unknown[];
    get(...params: unknown[]): unknown;
    /** node:sqlite: read integers as bigint (a stored value may exceed 2^53, e.g. a 2^62 "never" expiry) */
    setReadBigInts?(on: boolean): void;
  };
}

/** A statement of `db` that reads every integer as a bigint (values are copied as they are). */
const bigStatement = (db: SqlSrc, sql: string): ReturnType<SqlSrc['prepare']> => {
  const st = db.prepare(sql);
  st.setReadBigInts?.(true);
  return st;
};

export interface ImportResult extends CarryPair {
  /** fills copied */
  fills: number;
  /** fills already in the database (the same order and transaction) */
  duplicates: number;
  /** fills at or after `until` (the next run was live) */
  later: number;
  /** pair fills copied (as base or quote) */
  pairFills: number;
  /** first and last fill copied (ms) */
  first: number | null;
  last: number | null;
}

const hasColumn = (db: SqlSrc, table: string, col: string): boolean =>
  !!db.prepare(`SELECT 1 AS x FROM pragma_table_info('${table}') WHERE name = ?`).get(col);

const hexOf = (b: unknown): string => Buffer.from(b as Uint8Array).toString('hex');

/** The first fill of a database (ms, ignoring a zero time), any token: the start of that run's trading. */
export function firstFillTs(db: SqlSrc): number | null {
  const r = db.prepare("SELECT MIN(ts) AS t FROM order_events WHERE kind = 'fill' AND ts > 0").get() as { t: number | bigint | null };
  return r.t == null ? null : num(r.t);
}

/** Every token of a source database with KAS-book fills that maps by ticker to a current id (`old`: old id -> ticker). */
export function importPairs(src: SqlSrc, old: Map<string, string>, current: RegistryLike): CarryPair[] {
  const now = new Map((current.tokens ?? []).map((t) => [t.ticker, String(t.covenant_id).toLowerCase()]));
  const ids = src.prepare("SELECT DISTINCT lower(hex(token_cov_id)) AS t FROM order_events WHERE kind = 'fill' AND token_cov_id IS NOT NULL").all() as Array<{ t: string }>;
  const out: CarryPair[] = [];
  for (const { t } of ids) {
    const ticker = old.get(t) ?? [...now].find(([, id]) => id === t)?.[0];
    const to = ticker ? now.get(ticker) : undefined;
    if (ticker && to) out.push({ ticker, from: t, to });
  }
  return out.sort((a, b) => a.ticker.localeCompare(b.ticker) || a.from.localeCompare(b.from));
}

export function importHistory(
  dst: SqlDb,
  src: SqlSrc,
  pairs: CarryPair[],
  opts: { label: string; until: number; now?: () => number },
): { already: boolean; results: ImportResult[] } {
  for (const t of ['order_events', 'orders', 'order_state', 'pair_fills', 'meta']) {
    if (!dst.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?").get(t)) throw new Error(`not a KOB indexer database: no table ${t}`);
  }
  if (dst.prepare("SELECT 1 AS x FROM pragma_table_info('orders') WHERE name = 'lot_units'").get()) {
    throw new Error('the target database is of the lot layouts: open it with this build first');
  }
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(opts.label)) throw new Error(`bad label ${opts.label}`);
  const key = `history_import:${opts.label}`;
  if (dst.prepare('SELECT v FROM meta WHERE k = ?').get(key)) return { already: true, results: [] };
  const lots = hasColumn(src, 'orders', 'lot_units');
  const amount = lots ? 'e.lots * o.lot_units * o.unit' : 'e.amount';
  const fillSql = `SELECT e.covenant_id AS cov, e.daa, e.ts, e.txid, e.tx_pos, e.side, ${amount} AS amount, e.price, e.payout, e.closes, e.detail
     FROM order_events e JOIN orders o ON o.covenant_id = e.covenant_id
     WHERE e.kind = 'fill' AND e.token_cov_id = ? AND e.price IS NOT NULL AND ${amount} > 0 AND e.side IN (1, 2) ORDER BY e.ts, e.id`;
  const quote = hasColumn(src, 'orders', 'quote_cov_id') ? 'o.quote_cov_id' : 'NULL';
  const orderSql = lots
    ? `SELECT o.covenant_id, o.contract, o.template_hash, o.family, o.side, o.token_cov_id, o.token_tpl_hash, o.ext_commit, o.unit AS scale,
         o.lot_units * o.unit AS min_fill, o.price, NULL AS tip, o.tif, o.expiry_daa, o.active_from, o.in_book, NULL AS budget_rate, o.reserve,
         o.initial_lots * o.lot_units * o.unit AS initial_amount, o.deadline, o.genesis_state, o.genesis_txid, o.genesis_out, o.genesis_daa, o.parent,
         ${quote} AS quote_cov_id, s.status, s.filled_lots * o.lot_units * o.unit AS filled_amount, s.last_daa
       FROM orders o LEFT JOIN order_state s ON s.covenant_id = o.covenant_id WHERE o.covenant_id = ?`
    : `SELECT o.covenant_id, o.contract, o.template_hash, o.family, o.side, o.token_cov_id, o.token_tpl_hash, o.ext_commit, o.scale, o.min_fill,
         o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, o.reserve, o.initial_amount, o.deadline, o.genesis_state,
         o.genesis_txid, o.genesis_out, o.genesis_daa, o.parent, ${quote} AS quote_cov_id, s.status, s.filled_amount, s.last_daa
       FROM orders o LEFT JOIN order_state s ON s.covenant_id = o.covenant_id WHERE o.covenant_id = ?`;
  const orderOf = bigStatement(src, orderSql);
  const haveOrder = dst.prepare('SELECT 1 AS x FROM orders WHERE covenant_id = ?');
  // `+kind`: by the order's events (order_events_cov), not by every fill of the database (the planner's pick for `kind = ?`)
  const haveFill = dst.prepare("SELECT 1 AS x FROM order_events WHERE covenant_id = ? AND txid = ? AND +kind = 'fill'");
  const putOrder = dst.prepare(
    `INSERT INTO orders (covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, ext_commit, scale, min_fill, price, tip, tif,
       expiry_daa, active_from, in_book, budget_rate, reserve, initial_amount, deadline, genesis_state, genesis_txid, genesis_out, genesis_block, genesis_daa,
       parent, listed, unlisted_reason, origin, quote_cov_id)
     VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 0, ?, 'chain', ?)`,
  );
  const putState = dst.prepare(
    `INSERT INTO order_state (covenant_id, status, filled_amount, remaining_amount, amount_exact, state_known, last_block, last_daa)
     VALUES (?, ?, ?, NULL, 0, 0, 0, ?)`,
  );
  const putFill = dst.prepare(
    `INSERT INTO order_events (covenant_id, block_seq, daa, ts, txid, tx_pos, kind, token_cov_id, side, amount, price, payout, closes, detail)
     VALUES (?, 0, ?, ?, ?, ?, 'fill', ?, ?, ?, ?, ?, ?, ?)`,
  );
  const results: ImportResult[] = [];
  dst.exec('BEGIN IMMEDIATE');
  try {
    for (const p of pairs) {
      if (!HEX32.test(p.from) || !HEX32.test(p.to)) throw new Error(`bad pair ${p.ticker}: ${p.from} -> ${p.to}`);
      const r: ImportResult = { ...p, fills: 0, duplicates: 0, later: 0, pairFills: 0, first: null, last: null };
      const copied = new Set<string>();
      for (const f of bigStatement(src, fillSql).all(hex(p.from)) as Array<Record<string, unknown>>) {
        const ts = num(f.ts as number);
        if (ts >= opts.until) {
          r.later++;
          continue;
        }
        if (haveFill.get(f.cov, f.txid)) {
          r.duplicates++;
          continue;
        }
        const cov = hexOf(f.cov);
        if (!copied.has(cov) && !haveOrder.get(f.cov)) {
          const o = orderOf.get(f.cov) as Record<string, unknown>;
          putOrder.run(
            o.covenant_id, o.contract, o.template_hash, o.family, o.side, o.token_cov_id, o.token_tpl_hash, o.ext_commit, o.scale, o.min_fill, o.price,
            o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, o.reserve, o.initial_amount, o.deadline, o.genesis_state,
            o.genesis_txid, o.genesis_out, o.genesis_daa, o.parent, key, o.quote_cov_id,
          );
          const status = o.status == null || o.status === 'open' || o.status === 'partial' ? 'closed' : String(o.status);
          putState.run(o.covenant_id, status, o.filled_amount ?? 0, o.last_daa ?? o.genesis_daa);
        }
        copied.add(cov);
        putFill.run(f.cov, f.daa, f.ts, f.txid, f.tx_pos, hex(p.to), f.side, f.amount, f.price, f.payout, f.closes ?? 0, f.detail);
        r.fills++;
        r.first = r.first == null ? ts : Math.min(r.first, ts);
        r.last = r.last == null ? ts : Math.max(r.last, ts);
      }
      results.push(r);
    }
    // pair fills (protocol v3 and later): both tokens mapped, before `until`, each (order, transaction) once
    if (hasColumn(src, 'pair_fills', 'base_cov_id')) {
      const to = new Map(pairs.map((p) => [p.from, p.to]));
      const havePair = dst.prepare('SELECT 1 AS x FROM pair_fills WHERE base_cov_id = ? AND quote_cov_id = ? AND ts = ? AND covenant_id = ? AND txid = ?');
      const putPair = dst.prepare(
        `INSERT INTO pair_fills (block_seq, daa, ts, txid, tx_pos, covenant_id, contract, base_cov_id, quote_cov_id, side, amount_a, amount_b, price, a_scale,
           tip_kas, counterparty, price_source) VALUES (0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      );
      for (const f of bigStatement(src, 'SELECT * FROM pair_fills WHERE ts < ? ORDER BY ts, id').all(opts.until) as Array<Record<string, unknown>>) {
        const a = to.get(hexOf(f.base_cov_id));
        const b = to.get(hexOf(f.quote_cov_id));
        if (!a || !b || havePair.get(hex(a), hex(b), f.ts, f.covenant_id, f.txid)) continue;
        putPair.run(
          f.daa, f.ts, f.txid, f.tx_pos, f.covenant_id, f.contract, hex(a), hex(b), f.side, f.amount_a, f.amount_b, f.price, f.a_scale, f.tip_kas,
          f.counterparty, f.price_source ?? 'none',
        );
        for (const r of results) if (r.to === a || r.to === b) r.pairFills++;
      }
    }
    const rec = { at: (opts.now ?? Date.now)(), until: opts.until, results: results.map(({ ticker, from, fills, pairFills, first, last }) => ({ ticker, from, fills, pairFills, first, last })) };
    dst.prepare('INSERT INTO meta (k, v) VALUES (?, ?)').run(key, JSON.stringify(rec));
    dst.exec('COMMIT');
  } catch (e) {
    dst.exec('ROLLBACK');
    throw e;
  }
  return { already: false, results };
}
