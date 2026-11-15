CREATE TABLE job_notices (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN (
        'job_approval', 'permission', 'question', 'succeeded', 'failed', 'interrupted'
    )),
    run_id TEXT REFERENCES job_attempts(run_id),
    request_event_seq INTEGER REFERENCES agent_events(seq),
    CHECK ((kind = 'job_approval') = (run_id IS NULL)),
    CHECK ((kind IN ('permission', 'question')) = (request_event_seq IS NOT NULL))
) STRICT;

CREATE TRIGGER notice_job_approval AFTER INSERT ON job_approvals
WHEN NEW.decision = 'pending'
BEGIN
    INSERT INTO job_notices (job_id, kind) VALUES (NEW.job_id, 'job_approval');
END;

-- Carry forward actionable approvals without replaying historical outcomes.
INSERT INTO job_notices (job_id, kind)
SELECT approval.job_id, 'job_approval' FROM job_approvals approval
JOIN jobs j ON j.id = approval.job_id
WHERE approval.decision = 'pending' AND j.queue_state <> 'cancelled'
ORDER BY j.sequence;

CREATE TRIGGER notice_run_permission AFTER INSERT ON agent_permissions
WHEN NEW.delivery = 'pending'
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id, request_event_seq)
    SELECT job_id, 'permission', run_id, NEW.request_event_seq
    FROM job_attempts WHERE run_id = NEW.run_id;
END;

CREATE TRIGGER notice_native_question AFTER INSERT ON agent_questions
WHEN NEW.route = 'native' AND NEW.delivery = 'pending'
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id, request_event_seq)
    SELECT job_id, 'question', run_id, NEW.request_event_seq
    FROM job_attempts WHERE run_id = NEW.run_id;
END;

CREATE TRIGGER notice_structured_question AFTER INSERT ON structured_question_continuations
WHEN NEW.continuation_run_id IS NULL
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id, request_event_seq)
    SELECT a.job_id, 'question', a.run_id, NEW.request_event_seq
    FROM agent_questions q JOIN job_attempts a ON a.run_id = q.run_id
    WHERE q.request_event_seq = NEW.request_event_seq AND q.delivery = 'pending';
END;

CREATE TRIGGER notice_job_outcome AFTER UPDATE OF status ON runs
WHEN OLD.status <> NEW.status AND NEW.status IN ('succeeded', 'failed', 'interrupted')
BEGIN
    INSERT INTO job_notices (job_id, kind, run_id)
    SELECT job_id, NEW.status, run_id FROM job_attempts a WHERE a.run_id = NEW.id
        AND (NEW.status <> 'interrupted' OR a.process_launch_requested = 1)
        AND (NEW.status <> 'succeeded' OR NOT EXISTS (
            SELECT 1 FROM agent_questions q
            WHERE q.run_id = NEW.id AND q.route = 'structured_turn' AND q.delivery = 'pending'
        ));
END;

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
        SELECT 1 FROM agent_questions q
        WHERE q.request_event_seq = n.request_event_seq AND q.delivery = 'pending'
          AND (
            (q.route = 'native' AND EXISTS (
                SELECT 1 FROM active_agent_attempts a WHERE a.run_id = q.run_id
            ))
            OR (q.route = 'structured_turn' AND EXISTS (
                SELECT 1 FROM structured_question_continuations s
                JOIN agent_attempts a ON a.run_id = q.run_id
                JOIN conversations c ON c.id = a.conversation_id
                WHERE s.request_event_seq = q.request_event_seq AND s.continuation_run_id IS NULL
                    AND c.active_context_id = s.context_id
            ))
          )
    )
    ELSE 1
END;
