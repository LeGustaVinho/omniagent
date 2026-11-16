CREATE TABLE project_delivery_milestone_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    milestone TEXT NOT NULL CHECK (milestone IN ('verified_commit', 'verified_merge')),
    destination_branch TEXT,
    CHECK (milestone != 'verified_merge' OR destination_branch IS NOT NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE task_delivery_milestone_pins (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    workflow_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    plan_revision TEXT NOT NULL,
    milestone TEXT NOT NULL CHECK (milestone IN ('verified_commit', 'verified_merge')),
    candidate BLOB NOT NULL,
    PRIMARY KEY (workflow_run_id, plan_revision),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;
