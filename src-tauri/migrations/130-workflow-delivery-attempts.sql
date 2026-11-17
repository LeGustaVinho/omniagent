CREATE TABLE workflow_delivery_attempts (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE,
    plan_revision TEXT NOT NULL REFERENCES workflow_artifacts(id),
    node_instance_id TEXT NOT NULL,
    unit_id TEXT NOT NULL REFERENCES workflow_delivery_units(unit_id),
    ordinal INTEGER NOT NULL CHECK(ordinal > 0),
    attempt_number INTEGER NOT NULL CHECK(attempt_number > 0),
    PRIMARY KEY (pipeline_run_id, plan_revision, node_instance_id),
    UNIQUE (pipeline_run_id, attempt_number),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;
