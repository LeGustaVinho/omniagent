CREATE UNIQUE INDEX checkouts_project_checkout ON checkouts(project_id, id);
CREATE UNIQUE INDEX conversation_contexts_owner ON conversation_contexts(conversation_id, id);

CREATE TABLE managed_task_worktrees (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    checkout_id TEXT NOT NULL UNIQUE,
    branch TEXT NOT NULL,
    base_commit TEXT NOT NULL,
    PRIMARY KEY (project_id, task_id, checkout_id),
    UNIQUE (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE jobs (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    checkout_id TEXT NOT NULL,
    task_id TEXT,
    writer_checkout_id TEXT,
    conversation_id TEXT,
    context_id TEXT,
    origin_run_id TEXT REFERENCES agent_attempts(run_id),
    request_id TEXT,
    mode TEXT NOT NULL CHECK (mode IN ('read_only', 'plan', 'approved_write')),
    request BLOB NOT NULL,
    queue_state TEXT NOT NULL DEFAULT 'queued' CHECK (queue_state IN ('queued', 'paused', 'cancelled')),
    priority INTEGER NOT NULL DEFAULT 0,
    UNIQUE (project_id, id),
    UNIQUE (origin_run_id, request_id),
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    FOREIGN KEY (project_id, task_id, writer_checkout_id) REFERENCES managed_task_worktrees(project_id, task_id, checkout_id),
    FOREIGN KEY (conversation_id, context_id) REFERENCES conversation_contexts(conversation_id, id),
    CHECK ((origin_run_id IS NULL) = (request_id IS NULL)),
    CHECK ((conversation_id IS NULL) = (context_id IS NULL)),
    CHECK ((mode = 'approved_write') = (writer_checkout_id IS NOT NULL)),
    CHECK (writer_checkout_id IS NULL OR (task_id IS NOT NULL AND writer_checkout_id = checkout_id))
) STRICT;

CREATE INDEX jobs_pending ON jobs(queue_state, priority DESC, sequence);

CREATE TABLE job_dependencies (
    project_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    dependency_id TEXT NOT NULL,
    PRIMARY KEY (project_id, job_id, dependency_id),
    FOREIGN KEY (project_id, job_id) REFERENCES jobs(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, dependency_id) REFERENCES jobs(project_id, id),
    CHECK (job_id <> dependency_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE job_attempts (
    job_id TEXT NOT NULL REFERENCES jobs(id),
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    run_id TEXT NOT NULL UNIQUE REFERENCES agent_attempts(run_id),
    previous_run_id TEXT REFERENCES job_attempts(run_id),
    PRIMARY KEY (job_id, ordinal)
) STRICT, WITHOUT ROWID;
