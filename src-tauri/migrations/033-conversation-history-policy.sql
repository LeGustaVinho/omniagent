ALTER TABLE conversations ADD COLUMN queue_history_policy TEXT
    CHECK (queue_history_policy IN ('review', 'recorded', 'cancel'));

CREATE TABLE project_queue_history_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    policy TEXT NOT NULL CHECK (policy IN ('review', 'recorded', 'cancel'))
) STRICT, WITHOUT ROWID;

CREATE TABLE job_history_bindings (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    pipeline_pin INTEGER NOT NULL CHECK (pipeline_pin IN (0, 1)),
    structured_questions INTEGER NOT NULL DEFAULT 0
        CHECK (structured_questions IN (0, 1) AND structured_questions <= pipeline_pin)
) STRICT, WITHOUT ROWID;

ALTER TABLE agent_attempts ADD COLUMN detached_context INTEGER NOT NULL DEFAULT 0
    CHECK (detached_context IN (0, 1));

DROP VIEW deliverable_job_notices;
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
    WHEN 'question' THEN j.queue_state <> 'cancelled' AND EXISTS (
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
                LEFT JOIN job_history_bindings h ON h.job_id = j.id AND h.pipeline_pin = 1
                LEFT JOIN job_pipeline_origins o ON o.job_id = h.job_id
                LEFT JOIN job_pipeline_runs p ON p.id = o.pipeline_run_id
                WHERE s.request_event_seq = q.request_event_seq AND s.continuation_run_id IS NULL
                  AND (
                    c.active_context_id = s.context_id
                    OR (a.detached_context = 1 AND a.result_context_id = s.context_id
                        AND (h.job_id IS NULL OR (j.mode = 'read_only' AND p.plan_revision = o.plan_revision)))
                  )
            ))
          )
    )
    ELSE 1
END;
