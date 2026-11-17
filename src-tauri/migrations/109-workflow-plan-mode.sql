CREATE TABLE workflow_planning_conversations (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    conversation_id TEXT NOT NULL UNIQUE REFERENCES conversations(id),
    expected_plan_revision TEXT,
    max_subagents INTEGER NOT NULL CHECK (max_subagents BETWEEN 0 AND 32)
) STRICT;

DROP VIEW open_agent_questions;
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
LEFT JOIN workflow_structured_planning_jobs planning
    ON planning.job_id = j.job_id AND q.route = 'structured_turn'
LEFT JOIN workflow_planning_conversations configured
    ON configured.conversation_id = a.conversation_id AND EXISTS (
        SELECT 1 FROM job_pipeline_origins root WHERE root.job_id = configured.job_id
            AND root.project_id = o.project_id AND root.pipeline_run_id = o.pipeline_run_id
            AND root.node_instance_id = o.node_instance_id
    )
WHERE coalesce(job.queue_state, 'queued') <> 'cancelled'
    AND (configured.job_id IS NULL OR p.plan_revision IS configured.expected_plan_revision)
    AND (
        (w.state IN ('live', 'saving') AND q.delivery = 'pending'
            AND EXISTS (SELECT 1 FROM active_agent_attempts live WHERE live.run_id = a.run_id))
        OR (l.request_event_seq IS NOT NULL AND l.continuation_run_id IS NULL
            AND (l.context_id = c.active_context_id OR (
                a.detached_context = 1 AND a.result_context_id = l.context_id
                AND (coalesce(h.pipeline_pin, 0) = 0 OR p.plan_revision = o.plan_revision
                    OR planning.job_id IS NOT NULL OR configured.job_id IS NOT NULL)
            ) OR (h.pipeline_pin = 0 AND h.context_id = l.context_id AND answer.request_event_seq IS NOT NULL))
            AND (
                (q.route = 'structured_turn' AND q.delivery = 'pending')
                OR w.state IN ('saved', 'needs_review')
            ))
    );
