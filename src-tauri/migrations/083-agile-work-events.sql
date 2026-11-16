CREATE TABLE project_work_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created', 'state_changed', 'checklist_checked', 'checklist_reopened')),
    payload TEXT NOT NULL CHECK (json_type(payload) = 'object'),
    origin TEXT NOT NULL CHECK (json_type(origin) = 'object'),
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE
) STRICT;

CREATE INDEX project_work_events_by_project ON project_work_events (project_id, sequence);

CREATE TABLE pipeline_work_event_origins (
    pipeline_run_id TEXT PRIMARY KEY REFERENCES job_pipeline_runs(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL REFERENCES project_work_events(id),
    automation_chain TEXT NOT NULL CHECK (json_type(automation_chain) = 'array')
) STRICT, WITHOUT ROWID;
