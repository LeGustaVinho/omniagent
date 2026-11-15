CREATE TABLE context_snapshots (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE REFERENCES agent_attempts(run_id),
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE dispatches (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES turns(id),
    snapshot_id TEXT NOT NULL REFERENCES context_snapshots(id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
    frames BLOB NOT NULL,
    delivery TEXT NOT NULL CHECK (delivery IN ('prepared', 'submitted', 'unconfirmed')),
    UNIQUE (turn_id, ordinal)
) STRICT, WITHOUT ROWID;

ALTER TABLE agent_events ADD COLUMN source TEXT NOT NULL DEFAULT 'unknown'
    CHECK (source IN ('provider', 'application', 'supervisor', 'unknown'));
