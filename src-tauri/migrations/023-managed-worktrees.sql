CREATE TABLE project_worktree_roots (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    root TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE worktree_provisions (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    root TEXT NOT NULL UNIQUE,
    branch TEXT NOT NULL,
    base_commit TEXT NOT NULL,
    PRIMARY KEY (project_id, task_id),
    UNIQUE (project_id, branch),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE job_worktree_starts (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id),
    checkout_id TEXT NOT NULL REFERENCES managed_task_worktrees(checkout_id),
    state BLOB NOT NULL
) STRICT, WITHOUT ROWID;
