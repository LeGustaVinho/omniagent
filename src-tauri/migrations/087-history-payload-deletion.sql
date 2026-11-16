CREATE TABLE history_deleted_revisions (
    revision_id TEXT PRIMARY KEY REFERENCES message_revisions(id)
) STRICT, WITHOUT ROWID;

CREATE TABLE history_deleted_artifacts (
    artifact_id TEXT PRIMARY KEY REFERENCES conversation_artifact_revisions(id)
) STRICT, WITHOUT ROWID;

CREATE TABLE history_deleted_dispatch_payloads (
    dispatch_id TEXT PRIMARY KEY REFERENCES dispatches(id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE history_deleted_attempt_diffs (
    run_id TEXT PRIMARY KEY REFERENCES job_worktree_candidates(run_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE history_deleted_candidate_diffs (
    candidate_id TEXT PRIMARY KEY REFERENCES selected_candidate_revisions(id)
) STRICT, WITHOUT ROWID;

CREATE TABLE history_deleted_side_payloads (
    job_id TEXT PRIMARY KEY REFERENCES job_side_requests(job_id)
) STRICT, WITHOUT ROWID;
