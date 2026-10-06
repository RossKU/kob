-- KOB indexer schema (SQLite, WAL). Version recorded in meta.schema_version.
--
-- The database is a DERIVED view: the permanent source is the record log (`recordlog.rs`), and
-- `kob-executor index replay` rebuilds every table below from it.
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
CREATE TABLE IF NOT EXISTS orders (
    covenant_id   BLOB PRIMARY KEY,
    contract      TEXT NOT NULL,          -- KobAsk, KobBid, KobCondAsk, KobCondBid, KobIfdBid, KobIfdAsk (+ the ...Kron kinds), KobPair, KobCondPair, KobIfdPair
    template_hash BLOB NOT NULL,
    family        INTEGER NOT NULL,
    side          INTEGER NOT NULL,       -- 1: sells the token (ask side), 2: buys the token (bid side)
    maker         BLOB,
    token_cov_id  BLOB,
    token_tpl_hash BLOB,
    ext_commit    BLOB,                   -- KCC-20 extension commitment (bid kinds: state; ask kinds: placement custody)
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
    quote_cov_id  BLOB                    -- pair orders: the quote token B; token_cov_id is the base token A. NULL for every other kind
);
CREATE INDEX IF NOT EXISTS orders_book ON orders (token_cov_id, side, price);
CREATE INDEX IF NOT EXISTS orders_maker ON orders (maker);
CREATE INDEX IF NOT EXISTS orders_genesis_block ON orders (genesis_block);
CREATE INDEX IF NOT EXISTS orders_parent ON orders (parent);

-- Every unspent-or-spent output of an order covenant the indexer has seen.
CREATE TABLE IF NOT EXISTS order_utxos (
    txid          BLOB NOT NULL,
    idx           INTEGER NOT NULL,
    covenant_id   BLOB NOT NULL,
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
CREATE INDEX IF NOT EXISTS order_utxos_cov ON order_utxos (covenant_id);
CREATE INDEX IF NOT EXISTS order_utxos_created ON order_utxos (created_block);
CREATE INDEX IF NOT EXISTS order_utxos_spent ON order_utxos (spent_block);

-- Lifecycle events. kind: create | fill | cancel | amend | refund | kill | arm | trail | rearm | unknown
CREATE TABLE IF NOT EXISTS order_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    covenant_id BLOB NOT NULL,
    block_seq   INTEGER NOT NULL,
    daa         INTEGER NOT NULL,
    ts          INTEGER NOT NULL,
    txid        BLOB NOT NULL,
    tx_pos      INTEGER NOT NULL,         -- position of the transaction in the chain block
    kind        TEXT NOT NULL,
    token_cov_id BLOB,
    side        INTEGER,
    amount      INTEGER,                  -- base units (fill: filled; create: amountLeft; kill: returned; refund: left)
    price       INTEGER,                  -- sompi per whole token
    payout      INTEGER,                  -- sell side: value of the maker payout output (positional rule)
    closes      INTEGER NOT NULL DEFAULT 0,
    detail      TEXT
);
CREATE INDEX IF NOT EXISTS order_events_cov ON order_events (covenant_id, id);
CREATE INDEX IF NOT EXISTS order_events_block ON order_events (block_seq);
CREATE INDEX IF NOT EXISTS order_events_fills ON order_events (kind, token_cov_id, id);

-- In-place amends (AMEND record): the order row's terms BEFORE the amend of block `block_seq` (restored on revert).
CREATE TABLE IF NOT EXISTS order_amends (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    covenant_id     BLOB NOT NULL,
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
    reserve         INTEGER
);
CREATE INDEX IF NOT EXISTS order_amends_block ON order_amends (block_seq);

-- Derived, re-computable view of each order (see processor::refresh_order_state).
CREATE TABLE IF NOT EXISTS order_state (
    covenant_id    BLOB PRIMARY KEY,
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
CREATE INDEX IF NOT EXISTS order_state_status ON order_state (status);

-- Token UTXOs owned (scheme 0x04) by an order covenant id. role: custody = created by the
-- transaction that created or spent the order (exact custody, C7); stray = anything else sent to
-- the id (never liquidity, only the maker's cancel moves it; matcher.md 1.2).
CREATE TABLE IF NOT EXISTS token_utxos (
    txid          BLOB NOT NULL,
    idx           INTEGER NOT NULL,
    token_cov_id  BLOB NOT NULL,
    owner         BLOB NOT NULL,          -- the order covenant id
    amount        INTEGER NOT NULL,       -- token base units
    value         INTEGER NOT NULL,       -- KAS carrier
    role          TEXT NOT NULL,          -- custody | stray
    created_block INTEGER NOT NULL,
    created_daa   INTEGER NOT NULL,
    spent_block   INTEGER,
    spent_txid    BLOB,
    PRIMARY KEY (txid, idx)
);
CREATE INDEX IF NOT EXISTS token_utxos_owner ON token_utxos (owner);
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
    token_cov_id  BLOB NOT NULL,
    program       TEXT NOT NULL,          -- token program template (KCC20Ref_8x8, KronToken2433, ...)
    family        INTEGER NOT NULL,       -- 1 kcc20, 2 kron (KOB1 family byte)
    owner         BLOB NOT NULL,          -- the state's 32-byte owner (x-only key, script hash or covenant id)
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
CREATE INDEX IF NOT EXISTS token_holdings_owner ON token_holdings (owner, token_cov_id);
CREATE INDEX IF NOT EXISTS token_holdings_token ON token_holdings (token_cov_id, owner);
CREATE INDEX IF NOT EXISTS token_holdings_created ON token_holdings (created_block);
CREATE INDEX IF NOT EXISTS token_holdings_spent ON token_holdings (spent_block);

-- "Possibly frozen" annotations (an operational flag, not chain data: the co-located matcher writes it when the engine
-- pre-simulation of the order's next fill is rejected by the token program, e.g. a frozen or blacklisted balance). A flag names the
-- order UTXO it was raised for and lapses the moment the order moves (fill, cancel, refund): only a flag on the CURRENT outpoint
-- counts. `index replay` does not need it; a lost row only means the matcher probes again.
CREATE TABLE IF NOT EXISTS order_flags (
    covenant_id BLOB PRIMARY KEY,
    txid        BLOB NOT NULL,
    idx         INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    set_daa     INTEGER NOT NULL
);

-- Market reads: candles and 24 h stats select fills by time window, and the API asks for the newest event time.
CREATE INDEX IF NOT EXISTS order_events_fill_ts ON order_events (kind, token_cov_id, ts);
CREATE INDEX IF NOT EXISTS order_events_ts ON order_events (ts);
CREATE INDEX IF NOT EXISTS orders_token_genesis ON orders (token_cov_id, genesis_block, covenant_id);

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
