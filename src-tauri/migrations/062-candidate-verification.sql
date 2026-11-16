CREATE TABLE candidate_gate_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    revision TEXT NOT NULL,
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE selected_candidate_revisions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    unit_id TEXT NOT NULL,
    capture_run_id TEXT NOT NULL REFERENCES job_worktree_candidates(run_id),
    payload BLOB NOT NULL,
    UNIQUE (project_id, task_id, unit_id, id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE active_unit_candidates (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    unit_id TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    PRIMARY KEY (project_id, task_id, unit_id),
    FOREIGN KEY (project_id, task_id, unit_id, candidate_id)
        REFERENCES selected_candidate_revisions(project_id, task_id, unit_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE candidate_verifications (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    candidate_id TEXT NOT NULL REFERENCES selected_candidate_revisions(id),
    policy_revision TEXT NOT NULL,
    gates BLOB NOT NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at TEXT,
    UNIQUE (candidate_id, id)
) STRICT;

CREATE INDEX candidate_verifications_latest
    ON candidate_verifications(candidate_id, policy_revision, sequence DESC);

CREATE TABLE candidate_gate_results (
    verification_id TEXT NOT NULL REFERENCES candidate_verifications(id),
    gate_index INTEGER NOT NULL CHECK (gate_index >= 0),
    payload BLOB NOT NULL,
    PRIMARY KEY (verification_id, gate_index)
) STRICT, WITHOUT ROWID;

CREATE TABLE candidate_acceptances (
    id TEXT PRIMARY KEY,
    candidate_id TEXT NOT NULL,
    verification_id TEXT NOT NULL,
    accepted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (candidate_id, verification_id),
    FOREIGN KEY (candidate_id, verification_id) REFERENCES candidate_verifications(candidate_id, id)
) STRICT, WITHOUT ROWID;
