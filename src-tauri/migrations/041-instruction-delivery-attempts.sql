CREATE TABLE instruction_delivery_attempts (
    job_id TEXT NOT NULL REFERENCES queued_conversation_instructions(job_id),
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    policy_key TEXT NOT NULL,
    route TEXT NOT NULL CHECK (route IN ('standalone', 'continuation')),
    superseded INTEGER NOT NULL DEFAULT 0 CHECK (superseded IN (0, 1)),
    PRIMARY KEY (job_id, run_id)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX instruction_current_delivery
ON instruction_delivery_attempts(job_id) WHERE superseded = 0;

CREATE INDEX instruction_deliveries_by_run ON instruction_delivery_attempts(run_id, job_id);
