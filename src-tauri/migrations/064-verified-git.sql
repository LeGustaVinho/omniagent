CREATE TABLE project_delivery_git_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('automatic_commit_user_merge', 'user_commits_and_merges'))
) STRICT, WITHOUT ROWID;

CREATE TABLE delivery_commit_intents (
    acceptance_id TEXT PRIMARY KEY REFERENCES candidate_acceptances(id),
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    unit_id TEXT NOT NULL REFERENCES workflow_delivery_units(unit_id),
    payload BLOB NOT NULL,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE verified_delivery_commits (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    unit_id TEXT NOT NULL REFERENCES workflow_delivery_units(unit_id),
    checkout_id TEXT NOT NULL,
    acceptance_id TEXT NOT NULL UNIQUE REFERENCES candidate_acceptances(id),
    candidate_revision TEXT NOT NULL REFERENCES selected_candidate_revisions(id),
    verification_id TEXT NOT NULL REFERENCES candidate_verifications(id),
    capture_run_id TEXT NOT NULL REFERENCES job_attempts(run_id),
    sha TEXT NOT NULL,
    verified_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    payload BLOB NOT NULL,
    UNIQUE (project_id, sha),
    UNIQUE (project_id, task_id, unit_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id)
) STRICT, WITHOUT ROWID;
