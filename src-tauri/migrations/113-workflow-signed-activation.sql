CREATE TABLE workflow_maintenance_signed_activations (
    handoff_id TEXT PRIMARY KEY REFERENCES workflow_maintenance_handoffs(id),
    id TEXT NOT NULL UNIQUE,
    output_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_artifacts(id),
    receipt BLOB NOT NULL
) STRICT, WITHOUT ROWID;
