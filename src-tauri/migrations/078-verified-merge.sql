ALTER TABLE project_delivery_git_policies RENAME TO project_delivery_git_policies_previous;
CREATE TABLE project_delivery_git_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN (
        'automatic_commit_user_merge', 'user_commits_and_merges', 'automatic_local_commit_and_merge'
    ))
) STRICT, WITHOUT ROWID;
INSERT INTO project_delivery_git_policies (project_id, mode)
SELECT project_id, mode FROM project_delivery_git_policies_previous;
DROP TABLE project_delivery_git_policies_previous;

CREATE TABLE workflow_verified_merges (
    commit_evidence_id TEXT PRIMARY KEY REFERENCES verified_delivery_commits(id),
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    workflow_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    verified_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    payload BLOB NOT NULL,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;
CREATE INDEX workflow_verified_merges_history
ON workflow_verified_merges (project_id, task_id, workflow_run_id, verified_at DESC, id);

CREATE TABLE workflow_merge_intents (
    commit_evidence_id TEXT PRIMARY KEY REFERENCES verified_delivery_commits(id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE delivery_merge_previews (
    commit_evidence_id TEXT PRIMARY KEY REFERENCES verified_delivery_commits(id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;
