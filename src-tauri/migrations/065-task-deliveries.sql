CREATE TABLE task_delivery_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('manual', 'verified_commit', 'reopened', 'cancelled')),
    workflow_run_id TEXT,
    workflow_plan_revision TEXT,
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    payload BLOB,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    CHECK ((kind = 'verified_commit') = (workflow_run_id IS NOT NULL)),
    CHECK ((workflow_run_id IS NULL) = (workflow_plan_revision IS NULL)),
    CHECK ((kind IN ('manual', 'verified_commit')) = (payload IS NOT NULL))
) STRICT;

CREATE INDEX task_delivery_events_by_scope
    ON task_delivery_events(project_id, task_id, sequence DESC);
CREATE UNIQUE INDEX task_delivery_events_by_workflow
    ON task_delivery_events(workflow_run_id, workflow_plan_revision)
    WHERE workflow_run_id IS NOT NULL;
