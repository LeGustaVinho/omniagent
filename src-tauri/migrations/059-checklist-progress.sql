CREATE TABLE checklist_progress_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    before_revision INTEGER NOT NULL,
    after_revision INTEGER NOT NULL,
    checked INTEGER NOT NULL CHECK (checked IN (0, 1)),
    actor TEXT NOT NULL CHECK (actor IN ('operator', 'agent')),
    run_id TEXT REFERENCES agent_attempts(run_id) ON DELETE CASCADE,
    dispatch_id TEXT REFERENCES dispatches(id) ON DELETE CASCADE,
    request_id TEXT,
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
    CHECK (before_revision <= after_revision),
    CHECK ((actor = 'agent' AND run_id IS NOT NULL AND dispatch_id IS NOT NULL AND request_id IS NOT NULL)
           OR (actor = 'operator' AND run_id IS NULL AND dispatch_id IS NULL AND request_id IS NULL)),
    FOREIGN KEY (project_id, task_id, item_id, before_revision)
        REFERENCES project_checklist_item_revisions(project_id, task_id, item_id, revision) ON DELETE CASCADE,
    FOREIGN KEY (project_id, task_id, item_id, after_revision)
        REFERENCES project_checklist_item_revisions(project_id, task_id, item_id, revision) ON DELETE CASCADE,
    UNIQUE (run_id, request_id)
) STRICT;

CREATE INDEX checklist_progress_by_item
    ON checklist_progress_events(project_id, task_id, item_id, sequence);
