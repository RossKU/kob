-- Token-logo votes (`crate::logo_votes`; docs/design/logo-votes.md, docs/api-reference.md 3.19b). A vote is a wallet's signed
-- message, no transaction: up or down on a token's logo. The API publishes the totals per token only, never who voted.
--
-- Not derived from the chain: a database rebuilt by `kobd db replay` starts without these rows (`kobd logo-reports export` before,
-- `import` after; the export carries the votes). Added within schema 14 like `book_quotes` (`CREATE TABLE IF NOT EXISTS`): the
-- first start of a build that has them creates them, with no migration run and no version change.
--
-- Every accepted vote, kept for good (the founder: keep everything). `voter` is the x-only key of the P2PK address that signed,
-- `signature` the 64-byte Schnorr signature of the vote message, so each row can be checked again later. A change of mind is a
-- new row (`outcome = 'changed'`); the voter's current vote on a token is their newest row for it.
CREATE TABLE IF NOT EXISTS logo_votes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    covenant_id BLOB NOT NULL CHECK (length(covenant_id) = 32),
    voter       BLOB NOT NULL CHECK (length(voter) = 32),
    vote        INTEGER NOT NULL CHECK (vote IN (-1, 1)),
    -- the UTC day the message names (YYYY-MM-DD) and the proof-of-work nonce, as sent
    day         TEXT NOT NULL,
    nonce       TEXT NOT NULL,
    signature   BLOB NOT NULL CHECK (length(signature) = 64),
    created_ms  INTEGER NOT NULL,
    outcome     TEXT NOT NULL CHECK (outcome IN ('counted', 'changed'))
);
-- the voter's last vote (the 24-hour interval) and their current vote on a token
CREATE INDEX IF NOT EXISTS logo_votes_voter ON logo_votes (voter, created_ms);
CREATE INDEX IF NOT EXISTS logo_votes_pair ON logo_votes (covenant_id, voter, id);

-- The current totals per token: each voter's newest vote counted once. Kept in the transaction of every vote, and rebuilt
-- from `logo_votes` by an import.
CREATE TABLE IF NOT EXISTS logo_vote_totals (
    covenant_id BLOB PRIMARY KEY CHECK (length(covenant_id) = 32),
    up          INTEGER NOT NULL,
    down        INTEGER NOT NULL,
    updated_ms  INTEGER NOT NULL
) WITHOUT ROWID;
