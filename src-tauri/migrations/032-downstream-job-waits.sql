CREATE TABLE job_downstream_waits (
    id TEXT PRIMARY KEY,
    parent_run_id TEXT NOT NULL UNIQUE REFERENCES job_attempts(run_id),
    request_id TEXT NOT NULL,
    requested_at REAL NOT NULL DEFAULT (unixepoch('subsec')),
    state TEXT NOT NULL CHECK (state IN ('yielding', 'waiting', 'needs_review', 'continued', 'cancelled')),
    context_id TEXT REFERENCES conversation_contexts(id),
    continuation_run_id TEXT UNIQUE REFERENCES job_attempts(run_id),
    CHECK (state NOT IN ('waiting', 'continued') OR context_id IS NOT NULL),
    CHECK ((state = 'continued') = (continuation_run_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE job_downstream_inputs (
    wait_id TEXT NOT NULL REFERENCES job_downstream_waits(id),
    child_job_id TEXT NOT NULL REFERENCES jobs(id),
    ordinal INTEGER NOT NULL,
    child_run_id TEXT REFERENCES job_attempts(run_id),
    status TEXT CHECK (status IN ('succeeded', 'failed', 'cancelled', 'interrupted')),
    result_context_id TEXT REFERENCES conversation_contexts(id),
    PRIMARY KEY (wait_id, child_job_id),
    UNIQUE (wait_id, ordinal),
    CHECK (status IS NOT NULL OR (child_run_id IS NULL AND result_context_id IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX job_downstream_inputs_by_child ON job_downstream_inputs(child_job_id)
WHERE status IS NULL;

DROP TRIGGER notice_job_outcome;
CREATE TRIGGER notice_job_outcome AFTER UPDATE OF status ON runs
WHEN OLD.status <> NEW.status AND NEW.status IN ('succeeded', 'failed', 'interrupted')
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id)
    SELECT job_id, NEW.status, run_id FROM job_attempts a WHERE a.run_id = NEW.id
        AND (NEW.status <> 'interrupted' OR a.process_launch_requested = 1)
        AND (NEW.status <> 'succeeded' OR NOT EXISTS (
            SELECT 1 FROM agent_questions q
            WHERE q.run_id = a.run_id AND q.route = 'structured_turn' AND q.delivery = 'pending'
        ))
        AND (NEW.status = 'failed' OR NOT EXISTS (
            SELECT 1 FROM job_downstream_waits w WHERE w.parent_run_id = a.run_id AND w.state = 'yielding'
        ));
END;
