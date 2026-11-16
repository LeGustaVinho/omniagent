CREATE TABLE workflow_stage_assessments (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id) ON DELETE CASCADE,
    checked_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;
