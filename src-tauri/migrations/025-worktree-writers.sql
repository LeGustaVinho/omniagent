CREATE TABLE shared_worktree_scopes (
    project_id TEXT NOT NULL,
    scope_task_id TEXT NOT NULL,
    owner_task_id TEXT NOT NULL,
    checkout_id TEXT NOT NULL,
    PRIMARY KEY (project_id, scope_task_id),
    FOREIGN KEY (project_id, scope_task_id) REFERENCES project_tasks(project_id, id),
    FOREIGN KEY (project_id, owner_task_id, checkout_id)
        REFERENCES managed_task_worktrees(project_id, task_id, checkout_id),
    CHECK (scope_task_id <> owner_task_id)
) STRICT, WITHOUT ROWID;

-- jobs.task_id still identifies the owning worktree for the existing composite FK.
-- A shared-worktree job additionally records the scope that requested the work.
CREATE TABLE job_worktree_scopes (
    job_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    scope_task_id TEXT NOT NULL,
    FOREIGN KEY (project_id, job_id) REFERENCES jobs(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, scope_task_id) REFERENCES shared_worktree_scopes(project_id, scope_task_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE app_worktree_writers (
    checkout_id TEXT PRIMARY KEY REFERENCES managed_task_worktrees(checkout_id),
    operation_id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    scope_task_id TEXT NOT NULL,
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id),
    FOREIGN KEY (project_id, scope_task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER app_worktree_writer_exclusive BEFORE INSERT ON app_worktree_writers
WHEN EXISTS (SELECT 1 FROM job_writer_reservations WHERE checkout_id = NEW.checkout_id)
BEGIN
    SELECT RAISE(ABORT, 'worktree already has an agent writer');
END;

CREATE TRIGGER job_worktree_writer_exclusive BEFORE INSERT ON job_writer_reservations
WHEN EXISTS (SELECT 1 FROM app_worktree_writers WHERE checkout_id = NEW.checkout_id)
BEGIN
    SELECT RAISE(ABORT, 'worktree already has an app writer');
END;

CREATE TABLE pending_worktree_changes (
    checkout_id TEXT PRIMARY KEY REFERENCES managed_task_worktrees(checkout_id),
    project_id TEXT NOT NULL,
    scope_task_id TEXT NOT NULL,
    producing_run_id TEXT REFERENCES job_attempts(run_id),
    producing_operation_id TEXT,
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id),
    FOREIGN KEY (project_id, scope_task_id) REFERENCES project_tasks(project_id, id),
    CHECK ((producing_run_id IS NULL) <> (producing_operation_id IS NULL))
) STRICT, WITHOUT ROWID;

ALTER TABLE job_worktree_candidates ADD COLUMN capture_confirmed INTEGER NOT NULL DEFAULT 0
    CHECK (capture_confirmed IN (0, 1));
