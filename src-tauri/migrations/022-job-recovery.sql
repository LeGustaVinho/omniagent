ALTER TABLE agent_attempts ADD COLUMN result_context_id TEXT REFERENCES conversation_contexts(id);
ALTER TABLE job_attempts ADD COLUMN kind TEXT NOT NULL DEFAULT 'initial' CHECK (kind IN ('initial', 'retry', 'resume'));
ALTER TABLE job_attempts ADD COLUMN process_launch_requested INTEGER NOT NULL DEFAULT 0 CHECK (process_launch_requested IN (0, 1));

-- Older releases could launch the process before recording its running state.
UPDATE job_attempts SET process_launch_requested = 1
WHERE job_id IN (SELECT id FROM jobs WHERE mode = 'approved_write');

CREATE TABLE job_recoveries (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    source_run_id TEXT NOT NULL UNIQUE REFERENCES job_attempts(run_id),
    action TEXT NOT NULL CHECK (action IN ('retry', 'resume')),
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id)
) STRICT, WITHOUT ROWID;
