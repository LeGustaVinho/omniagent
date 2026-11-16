CREATE TABLE conversation_native_compactions (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id),
    submitted INTEGER NOT NULL DEFAULT 0 CHECK (submitted IN (0, 1)),
    detail TEXT
) STRICT, WITHOUT ROWID;
