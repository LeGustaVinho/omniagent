ALTER TABLE conversation_subagent_quotas ADD COLUMN native_requested INTEGER NOT NULL DEFAULT 0
    CHECK (native_requested BETWEEN 0 AND 32);
ALTER TABLE agent_sessions ADD COLUMN native_subagent_limit INTEGER
    CHECK (native_subagent_limit BETWEEN 0 AND 32);
ALTER TABLE agent_attempts ADD COLUMN subagent_conversation_id TEXT REFERENCES conversations(id);

UPDATE agent_attempts AS attempt SET subagent_conversation_id = coalesce((
    SELECT d.conversation_id FROM job_attempts a
    JOIN job_delegation_owners d ON d.job_id = a.job_id WHERE a.run_id = attempt.run_id
), attempt.conversation_id);

CREATE INDEX agent_attempts_by_subagent_conversation
    ON agent_attempts(subagent_conversation_id);

CREATE TABLE native_subagent_reservations (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id),
    session_id TEXT NOT NULL REFERENCES agent_sessions(id),
    requested INTEGER NOT NULL CHECK (requested BETWEEN 0 AND 32),
    state TEXT NOT NULL CHECK (state IN ('reserved', 'verified', 'needs_review', 'released'))
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX native_subagent_session_in_use
    ON native_subagent_reservations(session_id) WHERE state <> 'released';

DROP VIEW managed_subagent_states;
CREATE VIEW managed_subagent_states AS
SELECT d.conversation_id, j.id AS job_id,
       CASE
           WHEN EXISTS (
               SELECT 1 FROM job_attempts a
               LEFT JOIN active_agent_attempts live ON live.run_id = a.run_id
               LEFT JOIN native_subagent_reservations n ON n.run_id = a.run_id
               WHERE a.job_id = j.id AND (live.run_id IS NOT NULL OR n.state = 'needs_review')
           ) THEN 'active'
           WHEN j.queue_state <> 'cancelled' AND (
               NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
               OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id)
               OR EXISTS (
                   SELECT 1 FROM job_downstream_waits w JOIN job_attempts a ON a.run_id = w.parent_run_id
                   WHERE a.job_id = j.id AND w.state IN ('waiting', 'needs_review')
               )
           ) THEN 'queued'
           ELSE 'historical'
       END AS state
FROM job_delegation_owners d JOIN jobs j ON j.id = d.job_id;
