CREATE TABLE workflow_test_phase_actions (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    unit_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    request_key TEXT NOT NULL,
    inputs BLOB NOT NULL,
    output_id TEXT,
    retry_after INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, unit_id, plan_key, request_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_test_phase_actions_ready ON workflow_test_phase_actions(retry_after)
    WHERE output_id IS NULL;
