ALTER TABLE job_side_requests ADD COLUMN validation INTEGER NOT NULL DEFAULT 0 CHECK (validation IN (0, 1));

CREATE TABLE side_validation_evidence (
    run_id TEXT PRIMARY KEY REFERENCES job_side_responses(run_id),
    evidence_id TEXT NOT NULL UNIQUE REFERENCES capability_evidence(id),
    route TEXT NOT NULL CHECK (route IN ('codex_sample', 'fresh_codex_session', 'fresh_claude_session'))
) STRICT, WITHOUT ROWID;
