CREATE TABLE workflow_command_registrations (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    revision TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (project_id, id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_command_results (
    artifact_id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    run_id TEXT NOT NULL UNIQUE REFERENCES job_attempts(run_id) ON DELETE CASCADE,
    payload BLOB NOT NULL,
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_command_results_by_node
    ON workflow_command_results(pipeline_run_id, node_instance_id);
