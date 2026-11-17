CREATE TABLE workflow_github_source_syncs (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    repository_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    target_head TEXT NOT NULL,
    task_id TEXT NOT NULL,
    previous_head TEXT,
    accepted_sha TEXT NOT NULL,
    policy_revision TEXT NOT NULL,
    proof BLOB NOT NULL,
    PRIMARY KEY (project_id, repository_id, branch, target_head)
) STRICT, WITHOUT ROWID;
