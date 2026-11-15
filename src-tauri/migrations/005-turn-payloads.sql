CREATE TABLE fixture_inputs (
    turn_id TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
    payload BLOB NOT NULL,
    delivery TEXT NOT NULL CHECK (delivery IN ('prepared', 'sent')),
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT, WITHOUT ROWID;
