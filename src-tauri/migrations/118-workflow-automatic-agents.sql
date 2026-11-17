CREATE TABLE workflow_automatic_runs (
    pipeline_run_id TEXT PRIMARY KEY REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_automatic_agents (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    job_id TEXT,
    retry_after INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, request_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_automatic_agents_ready ON workflow_automatic_agents(retry_after)
    WHERE job_id IS NULL;
