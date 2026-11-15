CREATE TABLE native_goal_turns (
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id) ON DELETE CASCADE,
    provider_turn_id TEXT NOT NULL,
    started_event_seq INTEGER NOT NULL REFERENCES agent_events(seq) ON DELETE CASCADE,
    finished_event_seq INTEGER REFERENCES agent_events(seq) ON DELETE CASCADE,
    outcome TEXT CHECK (outcome IN ('completed', 'failed', 'interrupted')),
    CHECK ((outcome IS NULL) = (finished_event_seq IS NULL)),
    PRIMARY KEY (run_id, provider_turn_id)
);

CREATE UNIQUE INDEX native_goal_open_turn ON native_goal_turns(run_id) WHERE outcome IS NULL;

ALTER TABLE goal_revision_controls ADD COLUMN stop_event_floor INTEGER NOT NULL DEFAULT 0
    CHECK (stop_event_floor >= 0);

CREATE TABLE native_goal_controls (
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id) ON DELETE CASCADE,
    request_key TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('start', 'status', 'pause', 'stop', 'resume', 'cancel')),
    prepared_event_seq INTEGER NOT NULL REFERENCES agent_events(seq) ON DELETE CASCADE,
    settled_event_seq INTEGER REFERENCES agent_events(seq) ON DELETE CASCADE,
    outcome TEXT CHECK (outcome IN ('confirmed', 'rejected', 'unconfirmed')),
    CHECK ((outcome IS NULL) = (settled_event_seq IS NULL)),
    PRIMARY KEY (run_id, request_key)
);

CREATE TABLE native_goal_qualifications (
    launch_key TEXT PRIMARY KEY,
    evidence_id TEXT NOT NULL REFERENCES capability_evidence(id) ON DELETE CASCADE,
    qualification BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE native_goal_executors (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL REFERENCES goal_revisions(id) ON DELETE CASCADE,
    provider_session_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL REFERENCES capability_evidence(id),
    stop_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (stop_confirmed IN (0, 1)),
    provider_result TEXT CHECK (provider_result IN ('completed', 'blocked', 'limit_reached'))
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX live_native_goal_executor ON native_goal_executors(revision_id)
    WHERE stop_confirmed = 0;

CREATE TABLE native_goal_context_receipts (
    run_id TEXT PRIMARY KEY REFERENCES agent_attempts(run_id) ON DELETE CASCADE,
    provider_session_id TEXT NOT NULL,
    event_seq INTEGER NOT NULL REFERENCES agent_events(seq) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
