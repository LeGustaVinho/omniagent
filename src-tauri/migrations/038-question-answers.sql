DROP VIEW IF EXISTS deliverable_job_notices;
DROP VIEW IF EXISTS open_agent_questions;

CREATE TABLE question_continuation_links (
    request_event_seq INTEGER PRIMARY KEY REFERENCES agent_questions(request_event_seq),
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    continuation_run_id TEXT REFERENCES agent_attempts(run_id),
    reply_event_seq INTEGER UNIQUE REFERENCES agent_events(seq),
    CHECK ((continuation_run_id IS NULL) = (reply_event_seq IS NULL))
) STRICT;
INSERT INTO question_continuation_links SELECT * FROM structured_question_continuations;
DROP TABLE structured_question_continuations;
ALTER TABLE question_continuation_links RENAME TO structured_question_continuations;
CREATE INDEX question_continuations_by_run ON structured_question_continuations(continuation_run_id);

CREATE TABLE recorded_question_answers (
    request_event_seq INTEGER PRIMARY KEY REFERENCES structured_question_continuations(request_event_seq),
    accepted INTEGER NOT NULL CHECK (accepted IN (0, 1)),
    payload BLOB NOT NULL
) STRICT;

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
LEFT JOIN job_history_bindings h ON h.job_id = j.job_id
LEFT JOIN recorded_question_answers answer ON answer.request_event_seq = q.request_event_seq AND answer.accepted = 1
LEFT JOIN job_pipeline_origins o ON o.job_id = h.job_id
LEFT JOIN job_pipeline_runs p ON p.id = o.pipeline_run_id
WHERE coalesce(job.queue_state, 'queued') <> 'cancelled' AND (
    (w.state IN ('live', 'saving') AND q.delivery = 'pending'
        AND EXISTS (SELECT 1 FROM active_agent_attempts live WHERE live.run_id = a.run_id))
    OR (l.request_event_seq IS NOT NULL AND l.continuation_run_id IS NULL
        AND (l.context_id = c.active_context_id OR (
            a.detached_context = 1 AND a.result_context_id = l.context_id
            AND (coalesce(h.pipeline_pin, 0) = 0 OR p.plan_revision = o.plan_revision)
        ) OR (h.pipeline_pin = 0 AND h.context_id = l.context_id AND answer.request_event_seq IS NOT NULL))
        AND (
            (q.route = 'structured_turn' AND q.delivery = 'pending')
            OR w.state IN ('saved', 'needs_review')
        ))
);

CREATE VIEW ready_question_continuations AS
SELECT a.job_id, q.run_id, q.conversation_id, q.context_id
FROM open_agent_questions q JOIN job_attempts a ON a.run_id = q.run_id
LEFT JOIN recorded_question_answers answer ON answer.request_event_seq = q.request_event_seq AND answer.accepted = 1
WHERE q.context_id IS NOT NULL AND q.state IN ('saved', 'needs_review')
  AND a.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = a.job_id)
GROUP BY a.job_id, q.run_id, q.conversation_id, q.context_id
HAVING count(*) = count(answer.request_event_seq);

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
          AND NOT EXISTS (SELECT 1 FROM recorded_question_answers WHERE request_event_seq = q.request_event_seq)
    )
    ELSE 1
END;

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
