CREATE TABLE workflow_automatic_preparations (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    task_artifact_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    worktree_artifact_id TEXT,
    retry_after INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, decision_id),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_automatic_preparations_ready ON workflow_automatic_preparations(retry_after)
    WHERE worktree_artifact_id IS NULL;
