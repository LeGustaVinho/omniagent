CREATE TABLE workflow_goal_requalifications (
    revision_id TEXT PRIMARY KEY REFERENCES goal_revisions(id),
    predecessor_revision_id TEXT NOT NULL UNIQUE REFERENCES goal_revisions(id),
    checkpoint_run_id TEXT NOT NULL REFERENCES job_attempts(run_id),
    checkpoint_context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    prior_release_id TEXT NOT NULL REFERENCES job_pipeline_releases(id),
    release_id TEXT NOT NULL REFERENCES job_pipeline_releases(id),
    prior_turns INTEGER NOT NULL CHECK (prior_turns > 0),
    same_worktree INTEGER NOT NULL CHECK (same_worktree IN (0, 1)),
    requalified_ms INTEGER NOT NULL,
    CHECK (revision_id <> predecessor_revision_id AND prior_release_id <> release_id)
) STRICT, WITHOUT ROWID;
