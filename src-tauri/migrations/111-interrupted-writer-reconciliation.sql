CREATE TABLE interrupted_writer_reconciliations (
    source_run_id TEXT PRIMARY KEY REFERENCES job_worktree_starts(run_id),
    review_revision TEXT NOT NULL,
    external_effects_reconciled INTEGER NOT NULL CHECK (external_effects_reconciled = 1),
    reviewed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT, WITHOUT ROWID;
