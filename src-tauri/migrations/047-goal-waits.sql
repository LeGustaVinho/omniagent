ALTER TABLE goal_revision_controls ADD COLUMN resume_after_wait INTEGER NOT NULL DEFAULT 0
    CHECK (resume_after_wait IN (0, 1));

CREATE TABLE unconfirmed_managed_goal_stops (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE VIEW unconfirmed_agent_stops AS
SELECT run_id, conversation_id FROM unconfirmed_question_stops
UNION
SELECT held.run_id, a.conversation_id FROM unconfirmed_managed_goal_stops held
JOIN agent_attempts a ON a.run_id = held.run_id;
