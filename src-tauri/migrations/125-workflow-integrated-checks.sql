CREATE TABLE workflow_integrated_check_actions (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    request_key TEXT NOT NULL,
    integration_sha TEXT NOT NULL,
    inputs BLOB NOT NULL,
    gate_plan BLOB NOT NULL,
    output_id TEXT REFERENCES workflow_artifacts(id) ON DELETE CASCADE,
    retry_after INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, plan_key, request_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_integrated_check_actions_ready ON workflow_integrated_check_actions(retry_after)
    WHERE output_id IS NULL;
