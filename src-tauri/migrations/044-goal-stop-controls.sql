CREATE TABLE goal_revision_controls (
    revision_id TEXT PRIMARY KEY REFERENCES goal_revisions(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('active', 'pausing', 'paused', 'cancelling', 'cancelled', 'needs_review')),
    stop_action TEXT CHECK (stop_action IN ('pause', 'cancel')),
    stop_run_id TEXT REFERENCES runs(id),
    requested_ms INTEGER,
    settled_ms INTEGER,
    confirmed_by TEXT CHECK (confirmed_by IN ('recorded_idle', 'unlaunched', 'worker', 'operator')),
    CHECK (state NOT IN ('pausing', 'cancelling') OR (stop_action IS NOT NULL AND stop_run_id IS NOT NULL)),
    CHECK ((stop_action IS NULL) = (requested_ms IS NULL)),
    CHECK ((settled_ms IS NULL) = (confirmed_by IS NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO goal_revision_controls(revision_id, state)
SELECT r.id, CASE WHEN a.revision_id IS NOT NULL THEN 'active' ELSE 'cancelled' END
FROM goal_revisions r LEFT JOIN active_goals a ON a.revision_id = r.id;

CREATE TABLE goal_writer_recoveries (
    job_id TEXT PRIMARY KEY REFERENCES job_recoveries(job_id) ON DELETE CASCADE,
    checkpoint_run_id TEXT NOT NULL REFERENCES job_worktree_candidates(run_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE goal_writer_attempts (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id) ON DELETE CASCADE,
    checkpoint_run_id TEXT NOT NULL REFERENCES job_worktree_candidates(run_id)
) STRICT, WITHOUT ROWID;
