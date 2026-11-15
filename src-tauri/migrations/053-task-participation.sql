CREATE TABLE project_task_details (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    priority INTEGER NOT NULL,
    workflow_state TEXT NOT NULL CHECK (workflow_state IN (
        'backlog', 'ready', 'in_progress', 'in_review', 'blocked', 'completed', 'failed', 'cancelled'
    )),
    payload BLOB NOT NULL,
    PRIMARY KEY (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE project_task_prerequisites (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    prerequisite_id TEXT NOT NULL,
    PRIMARY KEY (project_id, task_id, prerequisite_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, prerequisite_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE,
    CHECK (task_id <> prerequisite_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX project_task_prerequisites_reverse
    ON project_task_prerequisites(project_id, prerequisite_id);

CREATE TABLE project_checklist_items (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (project_id, task_id, id),
    UNIQUE (project_id, task_id, position),
    FOREIGN KEY (project_id, task_id) REFERENCES project_checklists(project_id, task_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE project_checklist_item_revisions (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    body BLOB NOT NULL,
    checked INTEGER NOT NULL CHECK (checked IN (0, 1)),
    PRIMARY KEY (project_id, task_id, item_id, revision),
    FOREIGN KEY (project_id, task_id, item_id)
        REFERENCES project_checklist_items(project_id, task_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE job_task_run_scopes (
    job_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (length(trim(role)) > 0),
    checklist_item_id TEXT,
    FOREIGN KEY (project_id, job_id) REFERENCES jobs(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    FOREIGN KEY (project_id, task_id, checklist_item_id)
        REFERENCES project_checklist_items(project_id, task_id, id)
) STRICT, WITHOUT ROWID;

CREATE VIEW task_runs AS
SELECT j.project_id, coalesce(shared.scope_task_id, j.task_id) AS task_id,
       j.id AS job_id, attempt.ordinal, agent.run_id, agent.turn_id,
       target.role, target.checklist_item_id, session.scope, agent.reported_model,
       run.status, run.created_at, turn.closed_at, snapshot.id AS snapshot_id
FROM jobs j
JOIN job_attempts attempt ON attempt.job_id = j.id
JOIN agent_attempts agent ON agent.run_id = attempt.run_id
JOIN runs run ON run.id = agent.run_id
JOIN turns turn ON turn.id = agent.turn_id
JOIN agent_sessions session ON session.id = agent.agent_session_id
LEFT JOIN context_snapshots snapshot ON snapshot.run_id = agent.run_id
LEFT JOIN job_worktree_scopes shared ON shared.job_id = j.id
LEFT JOIN job_task_run_scopes target ON target.job_id = j.id
WHERE j.task_id IS NOT NULL;
