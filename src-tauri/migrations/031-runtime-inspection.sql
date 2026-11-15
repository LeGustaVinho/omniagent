ALTER TABLE agent_events ADD COLUMN inspect_diagnostic INTEGER NOT NULL DEFAULT 0 CHECK (inspect_diagnostic IN (0, 1));

CREATE INDEX agent_events_inspect_diagnostic ON agent_events(run_id, seq DESC)
WHERE inspect_diagnostic = 1;

CREATE TABLE agent_capability_summaries (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id),
    summary TEXT NOT NULL CHECK (json_valid(summary))
) STRICT, WITHOUT ROWID;
