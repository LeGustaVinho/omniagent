CREATE TABLE goal_criterion_evidence (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES goal_checkpoints(run_id) ON DELETE CASCADE,
    criterion_ordinal INTEGER NOT NULL CHECK (criterion_ordinal > 0),
    candidate_id TEXT REFERENCES selected_candidate_revisions(id),
    payload BLOB NOT NULL
) STRICT;

CREATE INDEX goal_criterion_evidence_by_result
    ON goal_criterion_evidence(run_id, criterion_ordinal, sequence DESC);
