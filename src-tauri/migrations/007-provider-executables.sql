CREATE TABLE cli_executables (
    provider TEXT PRIMARY KEY CHECK (provider IN ('codex', 'claude', 'antigravity')),
    executable TEXT NOT NULL
) STRICT, WITHOUT ROWID;
