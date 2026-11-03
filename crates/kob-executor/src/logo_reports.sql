-- Token-logo reports and the blocklist they feed (`crate::logo_reports`; docs/api.md, *Logo reports*). KOB keeps no image: a logo is loaded
-- by each viewer's browser from the address its creator gave, and a token listed here as hidden shows its letter instead.
--
-- Not derived from the chain: a database rebuilt by `index replay` starts without these tables' rows (`kob-executor index logo-reports export`
-- before, `import` after).
--
-- state: 'hidden'   hidden by a report; the operator may restore it
--        'restored' the operator restored it at `restored_ms`; reports inside the cooldown are recorded only, later ones hide it again
--        'never'    hidden by the operator for good (a report changes nothing; `restore` needs --force)
CREATE TABLE IF NOT EXISTS logo_blocklist (
    covenant_id  BLOB PRIMARY KEY CHECK (length(covenant_id) = 32),
    state        TEXT NOT NULL CHECK (state IN ('hidden', 'restored', 'never')),
    first_ms     INTEGER NOT NULL,
    hidden_ms    INTEGER NOT NULL,
    restored_ms  INTEGER,
    report_count INTEGER NOT NULL DEFAULT 0,
    -- when the set of hidden tokens last changed (`GET /v1/logo-blocklist` reports the largest)
    updated_ms   INTEGER NOT NULL
) WITHOUT ROWID;

-- One row per accepted report: no free text, nothing about the reporter. Bounded: the newest 50 000 are kept.
CREATE TABLE IF NOT EXISTS logo_reports (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    covenant_id BLOB NOT NULL CHECK (length(covenant_id) = 32),
    reason      TEXT NOT NULL CHECK (reason IN ('illegal', 'impersonation', 'other')),
    source      TEXT NOT NULL CHECK (source IN ('web', 'github')),
    ref         INTEGER,
    created_ms  INTEGER NOT NULL,
    -- 'hidden' (first report, or the token was already hidden), 'recorded' (inside the cooldown), 'rehidden' (after the cooldown)
    outcome     TEXT NOT NULL CHECK (outcome IN ('hidden', 'recorded', 'rehidden'))
);
CREATE INDEX IF NOT EXISTS logo_reports_covenant ON logo_reports (covenant_id, id);
