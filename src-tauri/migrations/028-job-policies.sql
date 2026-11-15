CREATE TABLE project_job_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    rules TEXT NOT NULL CHECK (json_valid(rules) AND json_type(rules) = 'array')
) STRICT, WITHOUT ROWID;

ALTER TABLE job_approvals ADD COLUMN policy_key TEXT NOT NULL DEFAULT 'default';

CREATE TABLE job_attempt_policies (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id),
    policy_key TEXT NOT NULL,
    rejection TEXT CHECK (rejection IS NULL OR json_valid(rejection))
) STRICT, WITHOUT ROWID;
