CREATE TABLE project_checklist_modes (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('track_progress', 'run_by_item'))
) STRICT, WITHOUT ROWID;

CREATE TABLE task_checklist_modes (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('track_progress', 'run_by_item')),
    PRIMARY KEY (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- The mode used by an existing job survives subsequent changes to its defaults.
CREATE TABLE job_checklist_modes (
    job_id TEXT PRIMARY KEY REFERENCES job_task_run_scopes(job_id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('track_progress', 'run_by_item'))
) STRICT, WITHOUT ROWID;
