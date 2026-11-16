CREATE TABLE workflow_subflow_pins (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    subflow_path TEXT NOT NULL,
    scope_key TEXT NOT NULL,
    artifact_kind TEXT NOT NULL CHECK (artifact_kind = 'subflow'),
    artifact_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    PRIMARY KEY (pipeline_run_id, subflow_path),
    FOREIGN KEY (scope_key, artifact_kind, artifact_id, revision)
      REFERENCES workflow_package_artifact_revisions(scope_key, kind, artifact_id, revision)
) STRICT, WITHOUT ROWID;
