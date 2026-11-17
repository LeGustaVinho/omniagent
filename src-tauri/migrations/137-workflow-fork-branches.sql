CREATE TABLE workflow_fork_branches (
    pipeline_run_id TEXT NOT NULL,
    fork_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    branch TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'completed', 'failed', 'skipped', 'cancelled')),
    settled_order INTEGER,
    PRIMARY KEY (pipeline_run_id, fork_id, plan_key, unit_key, branch),
    FOREIGN KEY (pipeline_run_id, fork_id)
      REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;
