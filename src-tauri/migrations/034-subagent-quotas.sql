CREATE TABLE project_subagent_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('managed', 'strict')),
    maximum INTEGER NOT NULL CHECK (maximum BETWEEN 0 AND 32)
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_subagent_quotas (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    mode TEXT CHECK (mode IN ('managed', 'strict')),
    requested INTEGER NOT NULL CHECK (requested BETWEEN 0 AND 32)
) STRICT, WITHOUT ROWID;

CREATE TABLE job_delegation_owners (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL REFERENCES conversations(id)
) STRICT, WITHOUT ROWID;

CREATE INDEX job_delegation_by_conversation ON job_delegation_owners(conversation_id);

CREATE VIEW managed_subagent_states AS
SELECT d.conversation_id, j.id AS job_id,
       CASE
           WHEN EXISTS (
               SELECT 1 FROM job_attempts a JOIN active_agent_attempts live ON live.run_id = a.run_id
               WHERE a.job_id = j.id
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

CREATE TABLE native_subagent_observations (
    event_seq INTEGER PRIMARY KEY REFERENCES agent_events(seq),
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    session_id TEXT NOT NULL REFERENCES agent_sessions(id),
    child_key BLOB CHECK (length(child_key) = 32),
    state TEXT CHECK (state IN ('active', 'queued', 'historical', 'unknown'))
) STRICT;

CREATE INDEX native_subagents_by_conversation
    ON native_subagent_observations(conversation_id, session_id, child_key, event_seq DESC);
