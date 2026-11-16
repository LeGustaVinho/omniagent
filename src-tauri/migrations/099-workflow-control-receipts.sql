CREATE TABLE workflow_control_receipts (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (pipeline_run_id, node_instance_id, plan_key, unit_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
      REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;
