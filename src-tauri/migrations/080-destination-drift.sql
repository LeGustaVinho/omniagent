CREATE TABLE project_destination_drift_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('pause', 'prepare_and_recheck'))
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_destination_drift_updates (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    workflow_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    commit_evidence_id TEXT NOT NULL REFERENCES verified_delivery_commits(id),
    destination_branch TEXT NOT NULL,
    expected_destination_sha TEXT NOT NULL,
    observed_destination_sha TEXT NOT NULL,
    payload BLOB NOT NULL,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    UNIQUE (commit_evidence_id, workflow_run_id, destination_branch, expected_destination_sha)
) STRICT, WITHOUT ROWID;
