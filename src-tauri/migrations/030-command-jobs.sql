-- Rebuild these tables with foreign keys disabled on the migration connection.
-- The migration runner checks every reference before committing.
CREATE TABLE runs_with_commands (
    id TEXT PRIMARY KEY,
    checkout_id TEXT NOT NULL REFERENCES checkouts(id),
    status TEXT NOT NULL CHECK (status IN (
        'starting', 'running', 'waiting_for_input', 'succeeded', 'failed', 'cancelled', 'interrupted'
    )),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    kind TEXT NOT NULL DEFAULT 'fixture' CHECK (kind IN ('fixture', 'agent', 'command'))
) STRICT, WITHOUT ROWID;
INSERT INTO runs_with_commands SELECT id, checkout_id, status, created_at, kind FROM runs;
DROP TABLE runs;
ALTER TABLE runs_with_commands RENAME TO runs;
CREATE INDEX runs_checkout ON runs(checkout_id, created_at);

CREATE TABLE executable_job_attempts (
    job_id TEXT NOT NULL REFERENCES jobs(id),
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    run_id TEXT NOT NULL UNIQUE REFERENCES runs(id),
    previous_run_id TEXT REFERENCES job_attempts(run_id),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    kind TEXT NOT NULL DEFAULT 'initial' CHECK (kind IN ('initial', 'retry', 'resume')),
    process_launch_requested INTEGER NOT NULL DEFAULT 0 CHECK (process_launch_requested IN (0, 1)),
    PRIMARY KEY (job_id, ordinal)
) STRICT, WITHOUT ROWID;
INSERT INTO executable_job_attempts
SELECT job_id, ordinal, run_id, previous_run_id, cancel_requested, kind, process_launch_requested
FROM job_attempts;
DROP TABLE job_attempts;
ALTER TABLE executable_job_attempts RENAME TO job_attempts;

CREATE TABLE executable_job_recoveries (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    source_run_id TEXT NOT NULL UNIQUE REFERENCES job_attempts(run_id),
    action TEXT NOT NULL CHECK (action IN ('retry', 'resume')),
    context_id TEXT REFERENCES conversation_contexts(id),
    CHECK (context_id IS NOT NULL OR action = 'retry')
) STRICT, WITHOUT ROWID;
INSERT INTO executable_job_recoveries SELECT job_id, source_run_id, action, context_id FROM job_recoveries;
DROP TABLE job_recoveries;
ALTER TABLE executable_job_recoveries RENAME TO job_recoveries;

CREATE TABLE job_commands (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    invocation BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE job_command_outputs (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id),
    output BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TRIGGER notice_job_outcome AFTER UPDATE OF status ON runs
WHEN OLD.status <> NEW.status AND NEW.status IN ('succeeded', 'failed', 'interrupted')
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id)
    SELECT job_id, NEW.status, run_id FROM job_attempts a WHERE a.run_id = NEW.id
        AND (NEW.status <> 'interrupted' OR a.process_launch_requested = 1)
        AND (NEW.status <> 'succeeded' OR NOT EXISTS (
            SELECT 1 FROM agent_questions q
            WHERE q.run_id = a.run_id AND q.route = 'structured_turn' AND q.delivery = 'pending'
        ));
END;
