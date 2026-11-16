CREATE TABLE workflow_launches (
    pipeline_run_id TEXT PRIMARY KEY REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    launch_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    UNIQUE (project_id, launch_key)
) STRICT, WITHOUT ROWID;
