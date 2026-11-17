CREATE TABLE workflow_agent_review_reuses (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    input_revision TEXT NOT NULL,
    output_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_artifacts(id),
    source_artifact_id TEXT NOT NULL REFERENCES workflow_artifacts(id),
    PRIMARY KEY (pipeline_run_id, node_instance_id, plan_key, unit_key, input_revision),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
      REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;
