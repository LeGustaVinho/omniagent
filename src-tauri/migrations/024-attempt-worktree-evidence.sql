CREATE TABLE job_worktree_candidates (
    run_id TEXT PRIMARY KEY REFERENCES job_worktree_starts(run_id),
    evidence BLOB NOT NULL
) STRICT, WITHOUT ROWID;
