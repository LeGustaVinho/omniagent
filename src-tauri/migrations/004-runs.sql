CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    checkout_id TEXT NOT NULL REFERENCES checkouts(id),
    status TEXT NOT NULL CHECK (status IN (
        'starting', 'running', 'waiting_for_input', 'succeeded', 'failed', 'cancelled', 'interrupted'
    )),
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT, WITHOUT ROWID;

CREATE INDEX runs_checkout ON runs(checkout_id, created_at);

CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
    opened_at INTEGER NOT NULL DEFAULT (unixepoch()),
    closed_at INTEGER,
    UNIQUE (run_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE run_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    turn_id TEXT NOT NULL REFERENCES turns(id),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE INDEX events_turn ON run_events(turn_id, seq);
