CREATE TABLE workflow_automatic_unit_actions (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_id TEXT NOT NULL REFERENCES workflow_delivery_units(unit_id),
    task_id TEXT NOT NULL,
    inputs BLOB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending', 'operator', 'published', 'failed')),
    output_id TEXT REFERENCES workflow_artifacts(id) ON DELETE SET NULL,
    retry_after INTEGER NOT NULL DEFAULT 0,
    diagnostic TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, request_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_automatic_unit_actions_ready ON workflow_automatic_unit_actions(retry_after)
    WHERE state = 'pending';

CREATE UNIQUE INDEX workflow_automatic_unit_actions_output
    ON workflow_automatic_unit_actions(pipeline_run_id, node_instance_id, plan_key, unit_id)
    WHERE state = 'published';
