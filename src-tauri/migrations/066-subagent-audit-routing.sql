CREATE TABLE attempt_subagent_audit_routing (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id),
    decision TEXT NOT NULL CHECK (json_type(decision) = 'object')
) STRICT, WITHOUT ROWID;
