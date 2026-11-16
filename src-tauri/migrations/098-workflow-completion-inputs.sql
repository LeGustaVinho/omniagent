CREATE TABLE workflow_completion_inputs (
    output_artifact_id TEXT NOT NULL REFERENCES workflow_artifacts(id) ON DELETE CASCADE,
    execution_key TEXT NOT NULL,
    input_revision TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (output_artifact_id, execution_key, input_revision)
) STRICT, WITHOUT ROWID;
