-- KOB indexer schema (SQLite, WAL). Version recorded in meta.schema_version.
--
-- The database is a DERIVED view: the permanent source is the record log (`recordlog.rs`), and
-- `kobd db replay` rebuilds every table below from it.
--
-- Identities (schemas 8 and 9): the 32-byte ids that the big tables repeat are stored once and referred to by integer. An order is
-- `orders.id` (AUTOINCREMENT: never reused, so no reverted order's id can name a later one); its covenant id stays in `orders` (the order of
-- the order lists is the order of the covenant ids). A token's covenant id, a holding's owner and (schema 9) the maker, template hashes,
-- extension commitment and quote token of an order are rows of `ids`, never deleted. The few values the orders name (a few dozen makers,
-- tokens and templates) have the first ids, so an order row and its index entries hold them in one or two bytes.
-- Rust still speaks `Hash32`: a statement resolves one with `(SELECT id FROM orders WHERE covenant_id = ?)` or
-- `(SELECT id FROM ids WHERE h = ?)`, and a read that returns one joins back (`indexer::ids`). The order lists are in the order of the
-- covenant ids, which is not the order of the integers: the order-list indexes below keep the covenant id as their last column, so that a page
-- is read in index order and never sorts (imported history is all genesis block 0: a sort of a block would sort tens of thousands of orders).
--
-- Reorg model: every mutable row is stamped with the chain block (`seq`) that created or spent it.
-- Reverting a chain block deletes what it created, un-spends what it spent, and re-derives
-- `order_state` for the affected covenants. Nothing else is ever updated in place, so a revert is
-- exact and a replay yields the same rows. `blocks` holds the chain blocks of the reorg / finality
-- window only (default 12 h); rows stamped with a pruned block are final.

CREATE TABLE IF NOT EXISTS meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
) WITHOUT ROWID;

-- Selected-chain blocks of the reorg window. `seq` is dense per applied block and NEVER reused
-- (AUTOINCREMENT): rows outlive the block row that stamped them.
CREATE TABLE IF NOT EXISTS blocks (
    seq  INTEGER PRIMARY KEY AUTOINCREMENT,
    hash BLOB NOT NULL UNIQUE,
    daa  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS blocks_daa ON blocks (daa);

-- One row per KOB order (a covenant lineage). Immutable after creation, except: `in_book` follows the tip state
-- (re-derived), and a maker's in-place amend (AMEND record, plain asks and bids) takes the new price, tip, tif, expiry_daa,
-- active_from, deadline and listing decision (a bid also its budget_rate and reserve); the replaced values go to
-- `order_amends`, stamped with the block, and a revert restores them (exact).
--
-- Quantities are token BASE UNITS; prices and tips are sompi per WHOLE token (`scale` base units of the order's token; a
-- pair order's price is in its state: token B base units per whole A).
CREATE TABLE IF NOT EXISTS ids (
    id INTEGER PRIMARY KEY,
    h  BLOB NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS orders (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    covenant_id   BLOB NOT NULL UNIQUE,
    contract      TEXT NOT NULL,          -- KobAsk, KobBid, KobCondAsk, KobCondBid, KobIfdBid, KobIfdAsk (+ the ...Kron kinds), KobPair, KobCondPair, KobIfdPair
    template_id   INTEGER NOT NULL,       -- ids.id of the order template hash
    family        INTEGER NOT NULL,
    side          INTEGER NOT NULL,       -- 1: sells the token (ask side), 2: buys the token (bid side)
    maker_id      INTEGER,                -- ids.id of the maker's key
    token_id      INTEGER,                -- ids.id of the token covenant id
    token_tpl_id  INTEGER,                -- ids.id of the token program template hash
    ext_id        INTEGER,                -- ids.id of the KCC-20 extension commitment (bid kinds: state; ask kinds: placement custody)
    scale         INTEGER,                -- base units per whole token (the price denominator)
    min_fill      INTEGER,                -- smallest fill in base units (unless the fill takes everything left)
    price         INTEGER,                -- quote, sompi per whole token; NULL for conditional kinds and pair orders
    tip           INTEGER,                -- priority tip, sompi per whole token
    tif           INTEGER,                -- 0 GTC/GTD, 1 IOC, 2 FOK (NULL for kinds without tif)
    expiry_daa    INTEGER,
    active_from   INTEGER,
    in_book       INTEGER NOT NULL,       -- 1 if the contract is a resting limit order (Ask, Bid, IfdAsk, IfdBid)
    budget_rate   INTEGER,                -- bids: pMax + tip, sompi per whole token (the escrow is consumed at it); buy-first entries: price + tip
    reserve       INTEGER,                -- bids: KAS never spent on fills
    initial_amount INTEGER,               -- base units at creation (amountLeft), NULL for bids (their quantity is their escrow)
    deadline      INTEGER,                -- day orders: UTC unix seconds of the placement record
    genesis_state BLOB NOT NULL,          -- state span as created
    genesis_txid  BLOB,
    genesis_out   INTEGER,
    genesis_block INTEGER NOT NULL,       -- blocks.seq; 0 = imported, never reverted
    genesis_daa   INTEGER NOT NULL,
    parent        BLOB,                   -- entry covenant id for if-done exits
    listed        INTEGER NOT NULL,
    unlisted_reason TEXT,
    origin        TEXT NOT NULL DEFAULT 'chain', -- chain | import
    quote_id      INTEGER,                -- pair orders: ids.id of the quote token B; token_id is the base token A. NULL for every other kind
    -- (schema 12) the KAS of the genesis output (the `order_utxos.value` of `genesis_txid`:`genesis_out`): a re-listing reads it
    -- (`processor::relist_proven`) once the archive pruning has deleted the spent UTXO row (`indexer::archive`). NULL: not known (a store
    -- migrated from an earlier schema whose genesis row was gone); the genesis UTXO row of such an order is never pruned.
    genesis_value INTEGER
);
CREATE INDEX IF NOT EXISTS orders_book ON orders (token_id, side, price);
-- a maker's orders newest first (`GET /v1/orders?maker=`, keyset paged): read in index order, one page per request
CREATE INDEX IF NOT EXISTS orders_maker_genesis ON orders (maker_id, genesis_block, covenant_id);
-- every order newest first (`GET /v1/orders` with no filter, keyset paged): the index is the whole sort order, so a page is read
-- in order and stops at its end, however many orders share a genesis block
CREATE INDEX IF NOT EXISTS orders_genesis_page ON orders (genesis_block, covenant_id);
CREATE INDEX IF NOT EXISTS orders_parent ON orders (parent) WHERE parent IS NOT NULL;

-- Every unspent-or-spent output of an order covenant the indexer has seen.
CREATE TABLE IF NOT EXISTS order_utxos (
    txid          BLOB NOT NULL,
    idx           INTEGER NOT NULL,
    order_id      INTEGER NOT NULL,       -- orders.id
    value         INTEGER NOT NULL,
    spk           BLOB NOT NULL,          -- version (2 bytes BE) + script, as the node reports it
    state         BLOB,                   -- state span when known (NULL after an underivable state change)
    created_block INTEGER NOT NULL,
    created_daa   INTEGER NOT NULL,
    spent_block   INTEGER,
    spent_txid    BLOB,
    spent_entry   TEXT,
    spent_amount  INTEGER,                -- a fill: base units filled
    PRIMARY KEY (txid, idx)
);
CREATE INDEX IF NOT EXISTS order_utxos_cov ON order_utxos (order_id);
CREATE INDEX IF NOT EXISTS order_utxos_created ON order_utxos (created_block);
CREATE INDEX IF NOT EXISTS order_utxos_spent ON order_utxos (spent_block);

-- Lifecycle events. kind: create | fill | cancel | amend | refund | kill | arm | trail | rearm | unknown
CREATE TABLE IF NOT EXISTS order_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id    INTEGER NOT NULL,         -- orders.id
    block_seq   INTEGER NOT NULL,
    daa         INTEGER NOT NULL,
    ts          INTEGER NOT NULL,
    txid        BLOB NOT NULL,
    tx_pos      INTEGER NOT NULL,         -- position of the transaction in the chain block
    kind        TEXT NOT NULL,
    token_id    INTEGER,                  -- ids.id of the token covenant id
    side        INTEGER,
    amount      INTEGER,                  -- base units (fill: filled; create: amountLeft; kill: returned; refund: left)
    price       INTEGER,                  -- sompi per whole token
    payout      INTEGER,                  -- sell side: value of the maker payout output (positional rule)
    closes      INTEGER NOT NULL DEFAULT 0,
    detail      TEXT,
    -- (schema 10) a fill: sompi it released to the filler, floor(amount x tip / scale) at the tip the order had at the fill
    -- (`activity::fill_fee`; NULL: a term unknown). NULL for every other kind.
    fee         INTEGER,
    -- (schema 10) a fill: scripts.id of the script that funded its transaction (the record log's payer section: an Executor's
    -- funding key). NULL: not recorded (a frame written before 2026-11-09, or no plain input funded it). NULL for every other kind.
    executor_id INTEGER
);
CREATE INDEX IF NOT EXISTS order_events_cov ON order_events (order_id, id);
CREATE INDEX IF NOT EXISTS order_events_block ON order_events (block_seq);
-- The fills only (about a third of the events): `kind = 'fill'` is in every read that wants them, so the planner takes this index for exactly those
-- (a store built before 2026-11 had them as `(kind, token_cov_id, id)` over every event; `kobd db migrate` builds the store anew).
CREATE INDEX IF NOT EXISTS order_events_fills ON order_events (token_id, id) WHERE kind = 'fill';
-- The in-place amends of an order (a few per order, if any): the price rule of the trades reads the latest one before a fill (`market::FILL_SELECT`),
-- so a fill of an order with a long history of fills does not walk that history back to the amend.
CREATE INDEX IF NOT EXISTS order_events_amends ON order_events (order_id, id) WHERE kind = 'amend';

-- In-place amends (AMEND record): the order row's terms BEFORE the amend of block `block_seq` (restored on revert).
CREATE TABLE IF NOT EXISTS order_amends (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id        INTEGER NOT NULL,     -- orders.id
    block_seq       INTEGER NOT NULL,
    price           INTEGER,
    tip             INTEGER,
    tif             INTEGER,
    expiry_daa      INTEGER,
    active_from     INTEGER,
    deadline        INTEGER,
    listed          INTEGER NOT NULL,
    unlisted_reason TEXT,
    -- a bid's budget rate and reserve before the amend (NULL for other kinds)
    budget_rate     INTEGER,
    reserve         INTEGER,
    -- the minimum fill before the amend (NULL in a row written before the column existed: nothing to restore)
    min_fill        INTEGER
);
CREATE INDEX IF NOT EXISTS order_amends_block ON order_amends (block_seq);

-- Derived, re-computable view of each order (see processor::refresh_order_state).
CREATE TABLE IF NOT EXISTS order_state (
    order_id       INTEGER PRIMARY KEY,   -- orders.id
    status         TEXT NOT NULL,         -- open | partial | filled | cancelled | refunded | killed | closed
    filled_amount  INTEGER NOT NULL,      -- base units filled (the sum of the fill events, saturating)
    remaining_amount INTEGER,             -- base units left (a bid: its buying power); NULL when unknown
    amount_exact   INTEGER NOT NULL DEFAULT 0, -- 1: read from the tip state (amountLeft), 0: estimated (bids)
    cur_txid       BLOB,
    cur_idx        INTEGER,
    cur_value      INTEGER,
    state_known    INTEGER NOT NULL,
    last_block     INTEGER NOT NULL,
    last_daa       INTEGER NOT NULL
);
-- The live set: `open` and `partial` are exactly the orders with a current (unspent) UTXO, `cur_txid` set
-- (`processor::refresh_order_state`). Every book, count and matcher read starts from it here, never from the order history.
CREATE INDEX IF NOT EXISTS order_state_status ON order_state (status);

-- Token UTXOs owned (scheme 0x04) by an order covenant id. role: custody = created by the
-- transaction that created or spent the order (exact custody, C7); stray = anything else sent to
-- the id (never liquidity, only the maker's cancel moves it; matcher.md 1.2).
CREATE TABLE IF NOT EXISTS token_utxos (
    txid          BLOB NOT NULL,
    idx           INTEGER NOT NULL,
    token_id      INTEGER NOT NULL,       -- ids.id of the token covenant id
    order_id      INTEGER NOT NULL,       -- orders.id of the owner: the order covenant id
    amount        INTEGER NOT NULL,       -- token base units
    value         INTEGER NOT NULL,       -- KAS carrier
    role          TEXT NOT NULL,          -- custody | stray
    created_block INTEGER NOT NULL,
    created_daa   INTEGER NOT NULL,
    spent_block   INTEGER,
    spent_txid    BLOB,
    PRIMARY KEY (txid, idx)
);
CREATE INDEX IF NOT EXISTS token_utxos_owner ON token_utxos (order_id);
-- The live custody of an order (`reads::CUSTODY_OK`, once per live ask in every book, count and stats read): strays sent to an order id
-- are not in it, so they cost those reads nothing.
CREATE INDEX IF NOT EXISTS token_utxos_custody_live ON token_utxos (order_id, amount) WHERE role = 'custody' AND spent_block IS NULL;
CREATE INDEX IF NOT EXISTS token_utxos_created ON token_utxos (created_block);
CREATE INDEX IF NOT EXISTS token_utxos_spent ON token_utxos (spent_block);

-- Token registry events: the first time a (token covenant id, token program, extension
-- commitment) identity appears in a placement record. Derived from the log; tiny.
CREATE TABLE IF NOT EXISTS token_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    token_cov_id BLOB NOT NULL,
    tpl_hash     BLOB,
    ext_commit   BLOB,
    kind         TEXT NOT NULL,           -- seen
    txid         BLOB NOT NULL,
    block_seq    INTEGER NOT NULL,
    daa          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS token_events_token ON token_events (token_cov_id);
-- The distinct programs (and commitments) a token's placements named, by skip-scan (`reads::claimed_programs`, `sole_extension`, the open
-- token list): an attacker adds a row per fresh commitment, so these reads must not grow with the rows.
CREATE INDEX IF NOT EXISTS token_events_token_tpl ON token_events (token_cov_id, tpl_hash, ext_commit) WHERE tpl_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS token_events_block ON token_events (block_seq);

-- KOB1 payloads that did not produce an order (bounded by chain content, revert-safe).
CREATE TABLE IF NOT EXISTS rejects (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    block_seq INTEGER NOT NULL,
    txid      BLOB NOT NULL,
    reason    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rejects_block ON rejects (block_seq);

-- Token UTXOs of tracked tokens (allowlisted, or traded by an order when the allowlist is not required) whose state the
-- indexer could PROVE: the creating transaction's transfer leader (KCC-20 `next_states`, KRON columns) reveals the state of
-- every output it authorises, and the state is kept only when the P2SH it produces equals the output's script public key.
-- Genesis outputs of an issuance are never listed (no transaction reveals their state until they are spent).
-- role: owned = any holder that is not a known order, custody / stray = owned by a KOB order id (see `token_utxos`).
CREATE TABLE IF NOT EXISTS token_holdings (
    txid          BLOB NOT NULL,
    idx           INTEGER NOT NULL,
    token_id      INTEGER NOT NULL,       -- ids.id of the token covenant id
    program       TEXT NOT NULL,          -- token program template (KCC20Ref_8x8, KronToken2433, ...)
    family        INTEGER NOT NULL,       -- 1 kcc20, 2 kron (KOB1 family byte)
    owner_id      INTEGER NOT NULL,       -- ids.id of the state's 32-byte owner (x-only key, script hash or covenant id)
    owner_kind    INTEGER NOT NULL,       -- kcc20 owner_scheme, kron id_type
    amount        INTEGER NOT NULL,       -- token base units
    value         INTEGER NOT NULL,       -- KAS carrier
    state         BLOB NOT NULL,          -- state span (112 bytes kcc20, 46 bytes kron)
    role          TEXT NOT NULL,          -- owned | custody | stray
    created_block INTEGER NOT NULL,
    created_daa   INTEGER NOT NULL,
    spent_block   INTEGER,
    spent_txid    BLOB,
    PRIMARY KEY (txid, idx)
);
-- The programs of one token's holdings, read by a skip-scan (MIN(program), then the next above it) for every evaluated order and every
-- open-list token (`reads::proven_programs`: 0.1 ms; the DISTINCT it replaced read all of a token's holdings, 18 s on the soak), and
-- asked once per inserted holding (`processor::insert_holding`: has the token a holding under this program yet). `holding_programs` is that
-- set with a count, kept by the triggers below in the transaction of every insert and delete of a holding (a batch, a revert, a replay;
-- not the pruning, schema 12): a few rows per token instead of the 190 MB `(token_cov_id, program)` index over every holding that served the same
-- two reads. `kobd db migrate` counts it once (`db::backfill_holding_programs`) for a store that did not have it.
CREATE TABLE IF NOT EXISTS holding_programs (
    token_id     INTEGER NOT NULL,        -- ids.id of the token covenant id
    program      TEXT NOT NULL,
    n            INTEGER NOT NULL,        -- holdings of the token under the program, spent ones included (pruned ones too, schema 12)
    PRIMARY KEY (token_id, program)
) WITHOUT ROWID;
CREATE TRIGGER IF NOT EXISTS holding_programs_insert AFTER INSERT ON token_holdings BEGIN
    INSERT INTO holding_programs (token_id, program, n) VALUES (NEW.token_id, NEW.program, 1)
        ON CONFLICT (token_id, program) DO UPDATE SET n = n + 1;
END;
-- (Schema 12) A delete under the `meta` key `pruning` (the lean and the archive pruning, which delete final spent holdings) takes nothing
-- back: a program the chain proved for a token stays proved once the holdings that proved it are gone (a revert, which never reaches a
-- pruned row, still takes back exactly what its block added). `n` is then the holdings ever counted, not the ones the store holds; every
-- read asks only whether the row exists.
CREATE TRIGGER IF NOT EXISTS holding_programs_delete AFTER DELETE ON token_holdings
    WHEN NOT EXISTS (SELECT 1 FROM meta WHERE k = 'pruning') BEGIN
    UPDATE holding_programs SET n = n - 1 WHERE token_id = OLD.token_id AND program = OLD.program;
    DELETE FROM holding_programs WHERE token_id = OLD.token_id AND program = OLD.program AND n <= 0;
END;
CREATE TRIGGER IF NOT EXISTS holding_programs_update AFTER UPDATE OF token_id, program ON token_holdings
    WHEN OLD.token_id IS NOT NEW.token_id OR OLD.program IS NOT NEW.program BEGIN
    UPDATE holding_programs SET n = n - 1 WHERE token_id = OLD.token_id AND program = OLD.program;
    DELETE FROM holding_programs WHERE token_id = OLD.token_id AND program = OLD.program AND n <= 0;
    INSERT INTO holding_programs (token_id, program, n) VALUES (NEW.token_id, NEW.program, 1)
        ON CONFLICT (token_id, program) DO UPDATE SET n = n + 1;
END;
-- The holdings of a token or of an owner in page order, spent ones included (`GET /v1/token-utxos?spent=true`): no sort of the lot.
-- (The token's page index replaces the `(token_cov_id, owner)` one, which no read used. An earlier store also had `(owner, token_cov_id)`:
-- the owner's page index serves the pair, in page order and without the sort; `kobd db migrate` does not build it.)
CREATE INDEX IF NOT EXISTS token_holdings_token_page ON token_holdings (token_id, created_block);
CREATE INDEX IF NOT EXISTS token_holdings_owner_page ON token_holdings (owner_id, created_block);
CREATE INDEX IF NOT EXISTS token_holdings_created ON token_holdings (created_block);
CREATE INDEX IF NOT EXISTS token_holdings_spent ON token_holdings (spent_block);
-- The unspent holdings of an owner or of a token, oldest first (`GET /v1/token-utxos`, the operator's own tokens): the live
-- rows only, in the order a page is read, however many spent rows the owner or the token has.
CREATE INDEX IF NOT EXISTS token_holdings_live_owner ON token_holdings (owner_id, created_block) WHERE spent_block IS NULL;
CREATE INDEX IF NOT EXISTS token_holdings_live_token ON token_holdings (token_id, created_block) WHERE spent_block IS NULL;

-- Maintained candle aggregates (`indexer::candles`): one row per bucket of the four intervals the API serves, written with the fills it
-- holds. Values are decimal text (i128). A token's rows are complete once it has a `candle_meta` row (the price basis they were built
-- at); `candle_dirty` holds the 1-minute buckets whose fills changed in the open transaction and is empty between transactions.
CREATE TABLE IF NOT EXISTS candles (
    token_cov_id  BLOB NOT NULL,
    iv            INTEGER NOT NULL,       -- 60000 | 300000 | 3600000 | 86400000 (ms)
    t             INTEGER NOT NULL,       -- bucket start (ms)
    o             TEXT NOT NULL,
    h             TEXT NOT NULL,
    l             TEXT NOT NULL,
    c             TEXT NOT NULL,
    volume        TEXT NOT NULL,
    quote_volume  TEXT NOT NULL,
    trades        INTEGER NOT NULL,
    PRIMARY KEY (token_cov_id, iv, t)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS candle_meta (
    token_cov_id  BLOB PRIMARY KEY,
    basis         INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS candle_dirty (
    token_cov_id  BLOB NOT NULL,
    t             INTEGER NOT NULL,
    PRIMARY KEY (token_cov_id, t)
) WITHOUT ROWID;

-- "Possibly frozen" annotations (an operational flag, not chain data: the co-located matcher writes it when the engine
-- pre-simulation of the order's next fill is rejected by the token program, e.g. a frozen or blacklisted balance). A flag names the
-- order UTXO it was raised for and lapses the moment the order moves (fill, cancel, refund): only a flag on the CURRENT outpoint
-- counts. `kobd db replay` does not need it; a lost row only means the matcher probes again.
CREATE TABLE IF NOT EXISTS order_flags (
    covenant_id BLOB PRIMARY KEY,
    txid        BLOB NOT NULL,
    idx         INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    set_daa     INTEGER NOT NULL
);

-- Market reads: candles and 24 h stats select fills by time window, and the API asks for the newest event time.
-- The fills only, as `order_events_fills` (earlier stores: `(kind, token_cov_id, ts)` over every event).
CREATE INDEX IF NOT EXISTS order_events_fill_ts ON order_events (token_id, ts) WHERE kind = 'fill';
CREATE INDEX IF NOT EXISTS order_events_ts ON order_events (ts);
CREATE INDEX IF NOT EXISTS orders_token_genesis ON orders (token_id, genesis_block, covenant_id);
-- the orders of a pair's quote token newest first (`GET /v1/orders?token=` lists a pair order under its token B too)
CREATE INDEX IF NOT EXISTS orders_quote_genesis ON orders (quote_id, genesis_block, covenant_id) WHERE quote_id IS NOT NULL;
-- The distinct programs the orders of a token pin, by skip-scan (`reads::claimed_programs`, `reads::open_standard_scale`): evaluated for
-- every order the indexer lists, so it must not read the token's orders (a bid-only token has hundreds of thousands).
CREATE INDEX IF NOT EXISTS orders_token_tpl ON orders (token_id, token_tpl_id) WHERE token_tpl_id IS NOT NULL;
-- The distinct (token, program, family) of the listed orders (the open token list): read one index seek per distinct row.
CREATE INDEX IF NOT EXISTS orders_listed_tokens ON orders (token_id, token_tpl_id, family)
    WHERE listed = 1 AND token_id IS NOT NULL AND token_tpl_id IS NOT NULL;

-- Pair-order fills (KobPair, KobCondPair, KobIfdPair): volume of the pair (A, B), never a KAS price. Prices, trades, candles and
-- last prices come ONLY from KAS-book order fills (`order_events` fills with a price); a pair fill records its event (price
-- NULL, the pair fields in `detail`) and this row. `counterparty`: route (the transaction also fills KAS-book orders of A or
-- B, which record their own trades), netting (an opposite pair order of the same pair filled in it, no KAS-book fill of A or
-- B), inventory (neither: the filler's own tokens). `price_source` is always `none`. Stamped with the block (reverted with it).
CREATE TABLE IF NOT EXISTS pair_fills (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    block_seq    INTEGER NOT NULL,
    daa          INTEGER NOT NULL,
    ts           INTEGER NOT NULL,
    txid         BLOB NOT NULL,
    tx_pos       INTEGER NOT NULL,
    covenant_id  BLOB NOT NULL,           -- the filled pair order
    contract     TEXT NOT NULL,
    base_cov_id  BLOB NOT NULL,           -- token A
    quote_cov_id BLOB NOT NULL,           -- token B
    side         INTEGER NOT NULL,        -- 1 the order sold A (ASK), 2 it bought A (BID)
    amount_a     INTEGER NOT NULL,        -- base units of A filled
    amount_b     INTEGER,                 -- base units of B the maker received (ASK) or paid (BID)
    price        INTEGER,                 -- the order's quote at the fill, B base units per whole A (`a_scale`)
    a_scale      INTEGER NOT NULL,
    tip_kas      INTEGER,                 -- sompi released to the filler
    counterparty TEXT NOT NULL,           -- route | netting | inventory
    price_source TEXT NOT NULL DEFAULT 'none'
);
CREATE INDEX IF NOT EXISTS pair_fills_pair ON pair_fills (base_cov_id, quote_cov_id, id);
CREATE INDEX IF NOT EXISTS pair_fills_pair_ts ON pair_fills (base_cov_id, quote_cov_id, ts);
CREATE INDEX IF NOT EXISTS pair_fills_block ON pair_fills (block_seq);

-- KCC-23 declarations (`kcc23.rs`). A candidate preparation: an accepted version 1 transaction whose payload is a declaration
-- (`0x00` + one strict I-JSON object, at most 16 KiB), its id-relevant fields in the record-log encoding (`kcc23::encode_tx`,
-- at most 32 KiB). Stamped with the block that accepted it (reverted with it); pruned once older than the declaration window
-- unless a declaration of the stored blocks names it.
CREATE TABLE IF NOT EXISTS kcc23_candidates (
    txid          BLOB PRIMARY KEY,
    tx            BLOB NOT NULL,
    created_block INTEGER NOT NULL,
    daa           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS kcc23_candidates_created ON kcc23_candidates (created_block);
CREATE INDEX IF NOT EXISTS kcc23_candidates_daa ON kcc23_candidates (daa);

-- The transactions of the verified declarations (the genesis and the preparations it binds), in the record-log encoding, once per
-- transaction id however many covenant ids a genesis creates. Deleted with the last declaration that names them.
CREATE TABLE IF NOT EXISTS kcc23_txs (
    txid BLOB PRIMARY KEY,
    tx   BLOB NOT NULL
);

-- A verified KCC-23 declaration per covenant id: the launch proof (`kob-kcc23-proof/1` JSON, `GET /v1/tokens/{id}/declaration`)
-- of the genesis that spent a candidate's output, assembled from `kcc23_txs` on read (`proof` is empty; a row an earlier build
-- wrote holds the whole JSON until `kcc23::compact_legacy` moves it). The first one stays; reverted with the genesis block.
CREATE TABLE IF NOT EXISTS kcc23_declarations (
    covenant_id      BLOB PRIMARY KEY,
    proof            TEXT NOT NULL,
    preparation_txid BLOB NOT NULL,
    genesis_txid     BLOB NOT NULL,
    genesis_block    INTEGER NOT NULL,       -- blocks.seq
    genesis_daa      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS kcc23_declarations_block ON kcc23_declarations (genesis_block);
CREATE INDEX IF NOT EXISTS kcc23_declarations_preparation ON kcc23_declarations (preparation_txid);
CREATE INDEX IF NOT EXISTS kcc23_declarations_genesis ON kcc23_declarations (genesis_txid);

-- Row counts `GET /v1/health` reports (`reads::counters`), kept by the triggers below in the transaction of every write
-- (a batch, a revert, a replay, an operator import), so a health check reads a few rows instead of counting the history.
-- name: orders | orders_listed | rejects | fills | status:<order_state.status>. A database without them gets them at
-- start-up, counted once (`db::backfill_counters`). (Schema 11) `orders_all`: the orders ever stored, which the lean pruning does not
-- take back (`GET /v1/stats` all time; a revert does); counted once from `orders` for a store that did not have it (`db::backfill_all_time`).
--
-- `order_state` is written with INSERT OR REPLACE: the replaced row's status leaves its count in the BEFORE INSERT trigger
-- (REPLACE fires no delete trigger while `recursive_triggers` is off, as every connection sets it, `db::pragmas`).
CREATE TABLE IF NOT EXISTS counters (
    name TEXT PRIMARY KEY,
    n    INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TRIGGER IF NOT EXISTS counters_orders_insert AFTER INSERT ON orders BEGIN
    INSERT INTO counters (name, n) VALUES ('orders', 1) ON CONFLICT (name) DO UPDATE SET n = n + 1;
    INSERT INTO counters (name, n) VALUES ('orders_all', 1) ON CONFLICT (name) DO UPDATE SET n = n + 1;
    INSERT INTO counters (name, n) VALUES ('orders_listed', NEW.listed = 1) ON CONFLICT (name) DO UPDATE SET n = n + excluded.n;
END;
CREATE TRIGGER IF NOT EXISTS counters_orders_delete AFTER DELETE ON orders BEGIN
    UPDATE counters SET n = n - 1 WHERE name = 'orders';
    UPDATE counters SET n = n - 1 WHERE name = 'orders_all' AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'pruning');
    UPDATE counters SET n = n - (OLD.listed = 1) WHERE name = 'orders_listed';
END;
CREATE TRIGGER IF NOT EXISTS counters_orders_listed AFTER UPDATE OF listed ON orders WHEN (OLD.listed = 1) != (NEW.listed = 1) BEGIN
    INSERT INTO counters (name, n) VALUES ('orders_listed', (NEW.listed = 1) - (OLD.listed = 1))
        ON CONFLICT (name) DO UPDATE SET n = n + excluded.n;
END;
CREATE TRIGGER IF NOT EXISTS counters_state_replace BEFORE INSERT ON order_state BEGIN
    UPDATE counters SET n = n - 1
        WHERE name = 'status:' || (SELECT s.status FROM order_state s WHERE s.order_id = NEW.order_id);
END;
CREATE TRIGGER IF NOT EXISTS counters_state_insert AFTER INSERT ON order_state BEGIN
    INSERT INTO counters (name, n) VALUES ('status:' || NEW.status, 1) ON CONFLICT (name) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS counters_state_delete AFTER DELETE ON order_state BEGIN
    UPDATE counters SET n = n - 1 WHERE name = 'status:' || OLD.status;
END;
CREATE TRIGGER IF NOT EXISTS counters_state_update AFTER UPDATE OF status ON order_state WHEN OLD.status IS NOT NEW.status BEGIN
    UPDATE counters SET n = n - 1 WHERE name = 'status:' || OLD.status;
    INSERT INTO counters (name, n) VALUES ('status:' || NEW.status, 1) ON CONFLICT (name) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS counters_rejects_insert AFTER INSERT ON rejects BEGIN
    INSERT INTO counters (name, n) VALUES ('rejects', 1) ON CONFLICT (name) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS counters_rejects_delete AFTER DELETE ON rejects BEGIN
    UPDATE counters SET n = n - 1 WHERE name = 'rejects';
END;
CREATE TRIGGER IF NOT EXISTS counters_fills_insert AFTER INSERT ON order_events WHEN NEW.kind = 'fill' BEGIN
    INSERT INTO counters (name, n) VALUES ('fills', 1) ON CONFLICT (name) DO UPDATE SET n = n + 1;
END;
CREATE TRIGGER IF NOT EXISTS counters_fills_delete AFTER DELETE ON order_events WHEN OLD.kind = 'fill' BEGIN
    UPDATE counters SET n = n - 1 WHERE name = 'fills';
END;
CREATE TRIGGER IF NOT EXISTS counters_fills_update AFTER UPDATE OF kind ON order_events WHEN (OLD.kind = 'fill') != (NEW.kind = 'fill') BEGIN
    INSERT INTO counters (name, n) VALUES ('fills', (NEW.kind = 'fill') - (OLD.kind = 'fill'))
        ON CONFLICT (name) DO UPDATE SET n = n + excluded.n;
END;

-- Schema 10: the activity totals (`GET /v1/stats`, `indexer::activity`), kept by the triggers below in the transaction of every write
-- (a batch, a revert, a replay, the lean pruning), so the totals of all time or of any window cost a few hundred bucket rows instead of
-- a scan of the history. A revert deletes the events and orders of its block, and the delete triggers take back exactly what the
-- insert triggers added (the fee is the event's own column). A store without them gets them counted once (`db::backfill_activity`).
--
-- Schema 11: the lean pruning (`indexer::lean`) deletes old rows inside a transaction that holds the `meta` key `pruning`, and the
-- delete triggers take nothing back while it is there: the totals are of everything the store ever indexed, not only of what it
-- still holds (a revert, which never reaches a pruned row, still takes back exactly what its block added). The key never outlives
-- the pruning's transaction.

-- The script public keys (version + script) that funded fill transactions, by integer; never deleted (like `ids`).
CREATE TABLE IF NOT EXISTS scripts (
    id  INTEGER PRIMARY KEY,
    spk BLOB NOT NULL UNIQUE
);

-- Per bucket of block time: the fill events, the tips they released, the `create` events (orders placed) and (schema 11) the
-- executors' transactions (`executor_txs`). Two bucket sizes, so a window from any minute reads the minutes up to its first whole
-- hour and the hours after it. A bucket row goes when it counts nothing.
CREATE TABLE IF NOT EXISTS activity (
    iv          INTEGER NOT NULL,              -- 60000 | 3600000 (ms)
    t           INTEGER NOT NULL,              -- bucket start (ms)
    fills       INTEGER NOT NULL,
    fees        INTEGER NOT NULL,              -- sompi: the priority tips of the fills (`order_events.fee`)
    orders      INTEGER NOT NULL,
    txs         INTEGER NOT NULL DEFAULT 0,    -- executors' transactions (schema 11), known or not
    income      INTEGER NOT NULL DEFAULT 0,    -- sompi they left to their senders (`executor_txs.income`), the known ones
    income_n    INTEGER NOT NULL DEFAULT 0,    -- transactions whose income is known
    miner_fees  INTEGER NOT NULL DEFAULT 0,    -- sompi of network fees (`executor_txs.miner_fee`), the known ones
    miner_fee_n INTEGER NOT NULL DEFAULT 0,    -- transactions whose network fee is known
    net         INTEGER NOT NULL DEFAULT 0,    -- sompi: income - network fee of the transactions where both are known
    net_n       INTEGER NOT NULL DEFAULT 0,    -- those transactions
    PRIMARY KEY (iv, t)
) WITHOUT ROWID;

-- Per maker (kind 1, ids.id of the key) and per executor (kind 2, scripts.id): the newest block (blocks.seq) of an order it placed or
-- of a fill it funded, so the distinct ones of a window are one index range. A delete takes it back to the newest one left (one seek
-- of `orders_maker_genesis` or `order_events_executor`), and drops it with its last order or fill unless the lean pruning deleted
-- some of its rows (`kept`, schema 11: it stays counted for all time, at block 0 once nothing of it is left).
CREATE TABLE IF NOT EXISTS actors (
    kind       INTEGER NOT NULL,
    actor_id   INTEGER NOT NULL,
    last_block INTEGER NOT NULL,
    kept       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (kind, actor_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS actors_last ON actors (kind, last_block);
CREATE INDEX IF NOT EXISTS order_events_executor ON order_events (executor_id, block_seq) WHERE kind = 'fill' AND executor_id IS NOT NULL;

CREATE TRIGGER IF NOT EXISTS activity_insert AFTER INSERT ON order_events WHEN NEW.kind IN ('fill', 'create') BEGIN
    INSERT INTO activity (iv, t, fills, fees, orders)
        SELECT v.iv, NEW.ts - NEW.ts % v.iv, NEW.kind = 'fill', CASE WHEN NEW.kind = 'fill' THEN COALESCE(NEW.fee, 0) ELSE 0 END,
               NEW.kind = 'create'
        FROM (SELECT 60000 AS iv UNION ALL SELECT 3600000) v WHERE true
        ON CONFLICT (iv, t) DO UPDATE SET fills = fills + excluded.fills, fees = fees + excluded.fees, orders = orders + excluded.orders;
END;
CREATE TRIGGER IF NOT EXISTS activity_delete AFTER DELETE ON order_events
    WHEN OLD.kind IN ('fill', 'create') AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'pruning') BEGIN
    UPDATE activity SET fills = fills - (OLD.kind = 'fill'), fees = fees - CASE WHEN OLD.kind = 'fill' THEN COALESCE(OLD.fee, 0) ELSE 0 END,
                        orders = orders - (OLD.kind = 'create')
        WHERE (iv = 60000 AND t = OLD.ts - OLD.ts % 60000) OR (iv = 3600000 AND t = OLD.ts - OLD.ts % 3600000);
    DELETE FROM activity WHERE ((iv = 60000 AND t = OLD.ts - OLD.ts % 60000) OR (iv = 3600000 AND t = OLD.ts - OLD.ts % 3600000))
        AND fills <= 0 AND orders <= 0 AND txs <= 0;
END;
CREATE TRIGGER IF NOT EXISTS activity_update AFTER UPDATE OF kind, ts, fee ON order_events
    WHEN (OLD.kind IN ('fill', 'create') OR NEW.kind IN ('fill', 'create'))
     AND (OLD.kind IS NOT NEW.kind OR OLD.ts IS NOT NEW.ts OR OLD.fee IS NOT NEW.fee) BEGIN
    UPDATE activity SET fills = fills - (OLD.kind = 'fill'), fees = fees - CASE WHEN OLD.kind = 'fill' THEN COALESCE(OLD.fee, 0) ELSE 0 END,
                        orders = orders - (OLD.kind = 'create')
        WHERE OLD.kind IN ('fill', 'create')
          AND ((iv = 60000 AND t = OLD.ts - OLD.ts % 60000) OR (iv = 3600000 AND t = OLD.ts - OLD.ts % 3600000));
    DELETE FROM activity WHERE ((iv = 60000 AND t = OLD.ts - OLD.ts % 60000) OR (iv = 3600000 AND t = OLD.ts - OLD.ts % 3600000))
        AND fills <= 0 AND orders <= 0 AND txs <= 0;
    INSERT INTO activity (iv, t, fills, fees, orders)
        SELECT v.iv, NEW.ts - NEW.ts % v.iv, NEW.kind = 'fill', CASE WHEN NEW.kind = 'fill' THEN COALESCE(NEW.fee, 0) ELSE 0 END,
               NEW.kind = 'create'
        FROM (SELECT 60000 AS iv UNION ALL SELECT 3600000) v WHERE NEW.kind IN ('fill', 'create')
        ON CONFLICT (iv, t) DO UPDATE SET fills = fills + excluded.fills, fees = fees + excluded.fees, orders = orders + excluded.orders;
END;

-- Schema 11: one row per transaction an executor sent (it spends a tracked order by anything but the maker's cancel: a fill, a
-- refund, a kill, an expiry, a keeper's arm or trail, a merge), stamped with its block (a revert deletes it) and never updated:
-- * `income`: what the transaction left to its sender before the network fee, in sompi: the KAS of the KOB order inputs it spent
--   (order UTXOs and their custody carriers) minus the KAS of the outputs the orders own or pay (maker payouts, refunds,
--   deliveries, continuations, custodies, exits; `indexer::income`), so the crossing spread, auction margins, priority tips and
--   keeper tips. NULL: unknown (an order input whose state is unproven, a maker's cancel in the same transaction, a spend this
--   build cannot classify, or a transaction a migration found already indexed);
-- * `miner_fee`: its network fee, what its inputs held (`record::TxRecord::in_value`, the node's own UTXO entries) minus its
--   outputs. NULL: unknown (a record written before 2026-11-12, or a node that did not report every input's value).
CREATE TABLE IF NOT EXISTS executor_txs (
    id        INTEGER PRIMARY KEY,
    block_seq INTEGER NOT NULL,
    daa       INTEGER NOT NULL,
    ts        INTEGER NOT NULL,
    txid      BLOB NOT NULL,
    income    INTEGER,
    miner_fee INTEGER
);
CREATE INDEX IF NOT EXISTS executor_txs_block ON executor_txs (block_seq);

CREATE TRIGGER IF NOT EXISTS executor_txs_insert AFTER INSERT ON executor_txs BEGIN
    INSERT INTO activity (iv, t, fills, fees, orders, txs, income, income_n, miner_fees, miner_fee_n, net, net_n)
        SELECT v.iv, NEW.ts - NEW.ts % v.iv, 0, 0, 0, 1, COALESCE(NEW.income, 0), NEW.income IS NOT NULL,
               COALESCE(NEW.miner_fee, 0), NEW.miner_fee IS NOT NULL, COALESCE(NEW.income - NEW.miner_fee, 0),
               NEW.income IS NOT NULL AND NEW.miner_fee IS NOT NULL
        FROM (SELECT 60000 AS iv UNION ALL SELECT 3600000) v WHERE true
        ON CONFLICT (iv, t) DO UPDATE SET txs = txs + 1, income = income + excluded.income, income_n = income_n + excluded.income_n,
            miner_fees = miner_fees + excluded.miner_fees, miner_fee_n = miner_fee_n + excluded.miner_fee_n,
            net = net + excluded.net, net_n = net_n + excluded.net_n;
END;
CREATE TRIGGER IF NOT EXISTS executor_txs_delete AFTER DELETE ON executor_txs
    WHEN NOT EXISTS (SELECT 1 FROM meta WHERE k = 'pruning') BEGIN
    UPDATE activity SET txs = txs - 1, income = income - COALESCE(OLD.income, 0), income_n = income_n - (OLD.income IS NOT NULL),
                        miner_fees = miner_fees - COALESCE(OLD.miner_fee, 0), miner_fee_n = miner_fee_n - (OLD.miner_fee IS NOT NULL),
                        net = net - COALESCE(OLD.income - OLD.miner_fee, 0),
                        net_n = net_n - (OLD.income IS NOT NULL AND OLD.miner_fee IS NOT NULL)
        WHERE (iv = 60000 AND t = OLD.ts - OLD.ts % 60000) OR (iv = 3600000 AND t = OLD.ts - OLD.ts % 3600000);
    DELETE FROM activity WHERE ((iv = 60000 AND t = OLD.ts - OLD.ts % 60000) OR (iv = 3600000 AND t = OLD.ts - OLD.ts % 3600000))
        AND fills <= 0 AND orders <= 0 AND txs <= 0;
END;

-- The actors' rows: the lean pruning marks the actor `kept` and leaves its block (the pruned row was real history); any other delete
-- takes it back to the newest row left, and drops it with its last row unless it is kept (then block 0: all time only).
CREATE TRIGGER IF NOT EXISTS actors_order_insert AFTER INSERT ON orders WHEN NEW.maker_id IS NOT NULL BEGIN
    INSERT INTO actors (kind, actor_id, last_block) VALUES (1, NEW.maker_id, NEW.genesis_block)
        ON CONFLICT (kind, actor_id) DO UPDATE SET last_block = MAX(last_block, excluded.last_block);
END;
CREATE TRIGGER IF NOT EXISTS actors_order_delete AFTER DELETE ON orders WHEN OLD.maker_id IS NOT NULL BEGIN
    UPDATE actors SET kept = 1 WHERE kind = 1 AND actor_id = OLD.maker_id AND EXISTS (SELECT 1 FROM meta WHERE k = 'pruning');
    DELETE FROM actors WHERE kind = 1 AND actor_id = OLD.maker_id AND kept = 0
        AND NOT EXISTS (SELECT 1 FROM orders WHERE maker_id = OLD.maker_id);
    UPDATE actors SET last_block = COALESCE((SELECT MAX(genesis_block) FROM orders WHERE maker_id = OLD.maker_id), 0)
        WHERE kind = 1 AND actor_id = OLD.maker_id AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'pruning');
END;
CREATE TRIGGER IF NOT EXISTS actors_order_update AFTER UPDATE OF maker_id, genesis_block ON orders
    WHEN OLD.maker_id IS NOT NEW.maker_id OR OLD.genesis_block IS NOT NEW.genesis_block BEGIN
    DELETE FROM actors WHERE kind = 1 AND actor_id = OLD.maker_id AND kept = 0
        AND NOT EXISTS (SELECT 1 FROM orders WHERE maker_id = OLD.maker_id);
    UPDATE actors SET last_block = COALESCE((SELECT MAX(genesis_block) FROM orders WHERE maker_id = OLD.maker_id), 0)
        WHERE kind = 1 AND actor_id = OLD.maker_id;
    INSERT INTO actors (kind, actor_id, last_block) SELECT 1, NEW.maker_id, NEW.genesis_block WHERE NEW.maker_id IS NOT NULL
        ON CONFLICT (kind, actor_id) DO UPDATE SET last_block = MAX(last_block, excluded.last_block);
END;
CREATE TRIGGER IF NOT EXISTS actors_fill_insert AFTER INSERT ON order_events WHEN NEW.kind = 'fill' AND NEW.executor_id IS NOT NULL BEGIN
    INSERT INTO actors (kind, actor_id, last_block) VALUES (2, NEW.executor_id, NEW.block_seq)
        ON CONFLICT (kind, actor_id) DO UPDATE SET last_block = MAX(last_block, excluded.last_block);
END;
CREATE TRIGGER IF NOT EXISTS actors_fill_delete AFTER DELETE ON order_events WHEN OLD.kind = 'fill' AND OLD.executor_id IS NOT NULL BEGIN
    UPDATE actors SET kept = 1 WHERE kind = 2 AND actor_id = OLD.executor_id AND EXISTS (SELECT 1 FROM meta WHERE k = 'pruning');
    DELETE FROM actors WHERE kind = 2 AND actor_id = OLD.executor_id AND kept = 0
        AND NOT EXISTS (SELECT 1 FROM order_events WHERE kind = 'fill' AND executor_id = OLD.executor_id);
    UPDATE actors SET last_block = COALESCE((SELECT MAX(block_seq) FROM order_events WHERE kind = 'fill' AND executor_id = OLD.executor_id), 0)
        WHERE kind = 2 AND actor_id = OLD.executor_id AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'pruning');
END;
CREATE TRIGGER IF NOT EXISTS actors_fill_update AFTER UPDATE OF kind, executor_id, block_seq ON order_events
    WHEN (OLD.kind = 'fill' AND OLD.executor_id IS NOT NULL) OR (NEW.kind = 'fill' AND NEW.executor_id IS NOT NULL) BEGIN
    DELETE FROM actors WHERE kind = 2 AND actor_id = OLD.executor_id AND kept = 0
        AND NOT EXISTS (SELECT 1 FROM order_events WHERE kind = 'fill' AND executor_id = OLD.executor_id);
    UPDATE actors SET last_block = COALESCE((SELECT MAX(block_seq) FROM order_events WHERE kind = 'fill' AND executor_id = OLD.executor_id), 0)
        WHERE kind = 2 AND actor_id = OLD.executor_id;
    INSERT INTO actors (kind, actor_id, last_block) SELECT 2, NEW.executor_id, NEW.block_seq
        WHERE NEW.kind = 'fill' AND NEW.executor_id IS NOT NULL
        ON CONFLICT (kind, actor_id) DO UPDATE SET last_block = MAX(last_block, excluded.last_block);
END;
