ALTER TABLE native_goal_executors ADD COLUMN token_budget INTEGER
    CHECK (token_budget IS NULL OR token_budget > 0);

CREATE TABLE native_goal_resume_checkpoints (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    source_run_id TEXT NOT NULL REFERENCES native_goal_executors(run_id),
    remaining_tokens INTEGER CHECK (remaining_tokens IS NULL OR remaining_tokens > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE late_goal_turn_reports (
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id) ON DELETE CASCADE,
    report_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (run_id, report_key)
) STRICT, WITHOUT ROWID;
