CREATE TABLE project_subagent_audit (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    policy TEXT NOT NULL CHECK (policy IN ('available', 'complete'))
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_subagent_audit (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    policy TEXT NOT NULL CHECK (policy IN ('available', 'complete'))
) STRICT, WITHOUT ROWID;

ALTER TABLE job_pipeline_nodes ADD COLUMN subagent_audit TEXT NOT NULL DEFAULT 'available'
    CHECK (subagent_audit IN ('available', 'complete'));

CREATE TABLE native_subagent_audit_candidates (
    launch_key TEXT PRIMARY KEY CHECK (length(launch_key) = 64),
    evidence_id TEXT NOT NULL UNIQUE REFERENCES capability_evidence(id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE attempt_subagent_audit (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id),
    route TEXT NOT NULL CHECK (route IN ('managed', 'native')),
    native_evidence_id TEXT REFERENCES capability_evidence(id),
    CHECK ((route = 'native') = (native_evidence_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

ALTER TABLE native_subagent_reservations ADD COLUMN strict_total INTEGER NOT NULL DEFAULT 1
    CHECK (strict_total IN (0, 1));
