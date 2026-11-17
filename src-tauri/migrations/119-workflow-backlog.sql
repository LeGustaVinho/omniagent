CREATE TABLE workflow_backlog_epics (
    pipeline_run_id TEXT PRIMARY KEY REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE,
    epic_id TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_backlog_tasks (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_backlog_epics(pipeline_run_id) ON DELETE CASCADE,
    task_key TEXT NOT NULL,
    task_id TEXT NOT NULL,
    PRIMARY KEY (pipeline_run_id, task_key),
    UNIQUE (pipeline_run_id, task_id)
) STRICT, WITHOUT ROWID;
