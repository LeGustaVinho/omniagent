CREATE TABLE workflow_automatic_delivery_results (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_id TEXT NOT NULL,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    attempt_id TEXT NOT NULL REFERENCES job_attempts(run_id) ON DELETE CASCADE,
    inputs BLOB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending', 'published', 'failed')),
    retry_after INTEGER NOT NULL DEFAULT 0,
    diagnostic TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, request_key, attempt_id),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_automatic_delivery_results_ready ON workflow_automatic_delivery_results(retry_after)
    WHERE state = 'pending';

CREATE TABLE workflow_automatic_delivery_outputs (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    port_name TEXT NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES workflow_artifacts(id) ON DELETE CASCADE,
    PRIMARY KEY (pipeline_run_id, node_instance_id, request_key, attempt_id, port_name),
    FOREIGN KEY (pipeline_run_id, node_instance_id, request_key, attempt_id)
        REFERENCES workflow_automatic_delivery_results(pipeline_run_id, node_instance_id, request_key, attempt_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
