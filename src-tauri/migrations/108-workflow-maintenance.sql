CREATE TABLE workflow_maintenance_settings (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    revision TEXT NOT NULL,
    configuration BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_maintenance_handoffs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    node_instance_id TEXT NOT NULL,
    git_artifact_id TEXT NOT NULL REFERENCES workflow_artifacts(id),
    commit_evidence_id TEXT NOT NULL REFERENCES verified_delivery_commits(id),
    settings_revision TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    receipt BLOB NOT NULL,
    UNIQUE (project_id, pipeline_run_id, commit_evidence_id, settings_revision),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_maintenance_fixture_activations (
    handoff_id TEXT PRIMARY KEY REFERENCES workflow_maintenance_handoffs(id),
    id TEXT NOT NULL UNIQUE,
    output_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_artifacts(id),
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT, WITHOUT ROWID;
