CREATE VIEW workflow_structured_planning_jobs AS
SELECT j.id AS job_id
FROM jobs j
JOIN job_history_bindings h ON h.job_id = j.id
    AND h.context_id = j.context_id AND h.pipeline_pin = 1 AND h.structured_questions = 1
JOIN conversation_contexts c ON c.id = h.context_id AND c.conversation_id = j.conversation_id
JOIN conversations chat ON chat.id = c.conversation_id AND chat.checkout_id = j.checkout_id
JOIN workflow_job_inputs i ON i.job_id = j.id AND i.unit_id IS NULL
JOIN job_pipeline_origins o ON o.job_id = j.id AND o.project_id = j.project_id
JOIN workflow_run_pins w ON w.pipeline_run_id = o.pipeline_run_id AND w.project_id = j.project_id
JOIN workflow_node_pins n ON n.pipeline_run_id = w.pipeline_run_id AND n.node_instance_id = o.node_instance_id
WHERE j.origin_run_id IS NULL AND j.mode = 'read_only'
  AND n.scheduler_profile = 'planning' AND n.requires_plan = 0
  AND o.plan_revision IS NULL AND o.release_id IS NULL;

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
WHERE coalesce(job.queue_state, 'queued') <> 'cancelled' AND (
    (w.state IN ('live', 'saving') AND q.delivery = 'pending'
        AND EXISTS (SELECT 1 FROM active_agent_attempts live WHERE live.run_id = a.run_id))
    OR (l.request_event_seq IS NOT NULL AND l.continuation_run_id IS NULL
        AND (l.context_id = c.active_context_id OR (
            a.detached_context = 1 AND a.result_context_id = l.context_id
            AND (coalesce(h.pipeline_pin, 0) = 0 OR p.plan_revision = o.plan_revision OR planning.job_id IS NOT NULL)
        ) OR (h.pipeline_pin = 0 AND h.context_id = l.context_id AND answer.request_event_seq IS NOT NULL))
        AND (
            (q.route = 'structured_turn' AND q.delivery = 'pending')
            OR w.state IN ('saved', 'needs_review')
        ))
);
