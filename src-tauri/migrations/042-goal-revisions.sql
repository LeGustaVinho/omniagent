CREATE TABLE goals (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    pipeline_run_id TEXT,
    node_instance_id TEXT,
    CHECK ((pipeline_run_id IS NULL) = (node_instance_id IS NULL)),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES job_pipeline_nodes(pipeline_run_id, instance_id) ON DELETE CASCADE
);

CREATE TABLE goal_revisions (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    initial_job_id TEXT UNIQUE NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    definition BLOB NOT NULL,
    continuation_owner TEXT NOT NULL CHECK (continuation_owner IN ('managed', 'native')),
    created_ms INTEGER NOT NULL,
    deadline_ms INTEGER CHECK (deadline_ms > created_ms),
    token_budget_requested INTEGER NOT NULL CHECK (token_budget_requested IN (0, 1)),
    UNIQUE (id, goal_id)
);

CREATE TABLE active_goals (
    goal_id TEXT PRIMARY KEY REFERENCES goals(id) ON DELETE CASCADE,
    revision_id TEXT UNIQUE NOT NULL,
    FOREIGN KEY (revision_id, goal_id) REFERENCES goal_revisions(id, goal_id) ON DELETE CASCADE
);

CREATE TABLE goal_continuations (
    revision_id TEXT NOT NULL REFERENCES goal_revisions(id) ON DELETE CASCADE,
    checkpoint_ordinal INTEGER NOT NULL CHECK (checkpoint_ordinal >= 0),
    job_id TEXT UNIQUE NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    PRIMARY KEY (revision_id, checkpoint_ordinal)
);

CREATE TABLE goal_checkpoints (
    revision_id TEXT NOT NULL REFERENCES goal_revisions(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    run_id TEXT UNIQUE NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id) ON DELETE CASCADE,
    next_action TEXT NOT NULL CHECK (next_action IN ('continue', 'review', 'blocked', 'limit_reached')),
    PRIMARY KEY (revision_id, ordinal)
);

CREATE TABLE goal_turn_reports (
    run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
    payload BLOB NOT NULL
);

CREATE TABLE native_goal_observations (
    run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
    event_seq INTEGER NOT NULL REFERENCES agent_events(seq) ON DELETE CASCADE,
    provider_goal_id TEXT,
    provider_session_id TEXT NOT NULL
);

CREATE UNIQUE INDEX conversation_goal_owner ON goals(conversation_id)
    WHERE pipeline_run_id IS NULL;
CREATE UNIQUE INDEX pipeline_goal_owner ON goals(pipeline_run_id, node_instance_id)
    WHERE pipeline_run_id IS NOT NULL;
