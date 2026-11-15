CREATE TABLE project_question_timeouts (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    minutes INTEGER NOT NULL CHECK (minutes BETWEEN 1 AND 120)
) STRICT, WITHOUT ROWID;

CREATE TABLE native_question_waits (
    request_event_seq INTEGER PRIMARY KEY REFERENCES agent_questions(request_event_seq),
    opened_ms INTEGER NOT NULL,
    deadline_ms INTEGER NOT NULL CHECK (deadline_ms >= opened_ms),
    state TEXT NOT NULL CHECK (state IN ('live', 'saving', 'saved', 'needs_review', 'closed')),
    stop_confirmed INTEGER CHECK (stop_confirmed IN (0, 1)),
    reason TEXT CHECK (reason IN ('timeout', 'provider_expired', 'turn_stopped', 'runtime_restart', 'transport_closed')),
    CHECK (state IN ('live', 'closed') OR reason IS NOT NULL)
) STRICT, WITHOUT ROWID;

INSERT INTO native_question_waits (request_event_seq, opened_ms, deadline_ms, state)
SELECT q.request_event_seq, unixepoch() * 1000, (unixepoch() + 600) * 1000, 'live'
FROM agent_questions q JOIN active_agent_attempts live ON live.run_id = q.run_id
WHERE q.route = 'native' AND q.delivery IN ('pending', 'prepared');

CREATE INDEX agent_questions_by_run ON agent_questions(run_id, request_event_seq);

CREATE VIEW unconfirmed_question_stops AS
SELECT DISTINCT q.run_id, a.conversation_id
FROM native_question_waits w JOIN agent_questions q ON q.request_event_seq = w.request_event_seq
JOIN agent_attempts a ON a.run_id = q.run_id WHERE w.stop_confirmed = 0;

CREATE VIEW open_agent_questions AS
SELECT q.request_event_seq, q.run_id, a.conversation_id, l.context_id,
       coalesce(w.state, 'saved') AS state,
       coalesce(w.reason, 'structured_output') AS reason
FROM agent_questions q
JOIN agent_attempts a ON a.run_id = q.run_id
JOIN conversations c ON c.id = a.conversation_id
LEFT JOIN native_question_waits w ON w.request_event_seq = q.request_event_seq
LEFT JOIN structured_question_continuations l ON l.request_event_seq = q.request_event_seq
LEFT JOIN job_attempts j ON j.run_id = q.run_id
LEFT JOIN jobs job ON job.id = j.job_id
LEFT JOIN job_history_bindings h ON h.job_id = j.job_id AND h.pipeline_pin = 1
LEFT JOIN job_pipeline_origins o ON o.job_id = h.job_id
LEFT JOIN job_pipeline_runs p ON p.id = o.pipeline_run_id
WHERE coalesce(job.queue_state, 'queued') <> 'cancelled' AND (
    (w.state IN ('live', 'saving') AND q.delivery = 'pending'
        AND EXISTS (SELECT 1 FROM active_agent_attempts live WHERE live.run_id = a.run_id))
    OR (l.request_event_seq IS NOT NULL AND l.continuation_run_id IS NULL
        AND (l.context_id = c.active_context_id OR (
            a.detached_context = 1 AND a.result_context_id = l.context_id
            AND (h.job_id IS NULL OR p.plan_revision = o.plan_revision)
        ))
        AND (
            (q.route = 'structured_turn' AND q.delivery = 'pending')
            OR w.state IN ('saved', 'needs_review')
        ))
);

DROP VIEW IF EXISTS deliverable_job_notices;
CREATE VIEW deliverable_job_notices AS
SELECT n.sequence, j.project_id, n.job_id, n.kind, n.run_id, n.request_event_seq
FROM job_notices n JOIN jobs j ON j.id = n.job_id
WHERE CASE n.kind
    WHEN 'job_approval' THEN j.queue_state <> 'cancelled' AND EXISTS (
        SELECT 1 FROM job_approvals p WHERE p.job_id = n.job_id AND p.decision = 'pending'
    )
    WHEN 'permission' THEN EXISTS (
        SELECT 1 FROM agent_permissions p JOIN active_agent_attempts a ON a.run_id = p.run_id
        WHERE p.request_event_seq = n.request_event_seq AND p.delivery = 'pending'
    )
    WHEN 'question' THEN EXISTS (
        SELECT 1 FROM open_agent_questions q WHERE q.request_event_seq = n.request_event_seq
          AND q.state IN ('live', 'saved')
    )
    ELSE 1
END;

DROP TRIGGER IF EXISTS notice_structured_question;
CREATE TRIGGER notice_structured_question AFTER INSERT ON structured_question_continuations
WHEN NEW.continuation_run_id IS NULL
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id, request_event_seq)
    SELECT a.job_id, 'question', a.run_id, NEW.request_event_seq
    FROM agent_questions q JOIN job_attempts a ON a.run_id = q.run_id
    WHERE q.request_event_seq = NEW.request_event_seq AND (
        (q.route = 'structured_turn' AND q.delivery = 'pending')
        OR EXISTS (SELECT 1 FROM native_question_waits w WHERE w.request_event_seq = q.request_event_seq AND w.state = 'saved')
    ) AND NOT EXISTS (SELECT 1 FROM job_notices WHERE request_event_seq = q.request_event_seq);
END;

DROP TRIGGER IF EXISTS notice_job_outcome;
CREATE TRIGGER notice_job_outcome AFTER UPDATE OF status ON runs
WHEN OLD.status <> NEW.status AND NEW.status IN ('succeeded', 'failed', 'interrupted')
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id)
    SELECT job_id, NEW.status, run_id FROM job_attempts a WHERE run_id = NEW.id
        AND (NEW.status <> 'interrupted' OR a.process_launch_requested = 1)
        AND NOT EXISTS (
            SELECT 1 FROM agent_questions q LEFT JOIN native_question_waits w ON w.request_event_seq = q.request_event_seq
            WHERE q.run_id = NEW.id AND (
                w.state = 'saving'
                OR (NEW.status = 'succeeded' AND q.route = 'structured_turn' AND q.delivery = 'pending')
            )
        );
END;
