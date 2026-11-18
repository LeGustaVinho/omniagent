CREATE TABLE IF NOT EXISTS deleted_workflow_authoring_jobs (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id)
) STRICT, WITHOUT ROWID;
