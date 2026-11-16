CREATE TABLE project_continuation_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    choice TEXT NOT NULL CHECK (choice IN ('same_tab', 'fork_with_context', 'fresh'))
);

CREATE TABLE conversation_continuation_policies (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    choice TEXT NOT NULL CHECK (choice IN ('same_tab', 'fork_with_context', 'fresh'))
);

CREATE TABLE conversation_continuation_previews (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL UNIQUE REFERENCES conversations(id) ON DELETE CASCADE,
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id) ON DELETE CASCADE,
    payload BLOB NOT NULL
);

CREATE TABLE conversation_continuation_jobs (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    seed_digest BLOB NOT NULL CHECK (length(seed_digest) = 32),
    native_session_id TEXT
);
