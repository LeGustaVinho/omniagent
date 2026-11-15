ALTER TABLE runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'fixture' CHECK (kind IN ('fixture', 'agent'));

CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    checkout_id TEXT NOT NULL REFERENCES checkouts(id),
    title TEXT NOT NULL,
    active_context_id TEXT REFERENCES conversation_contexts(id)
) STRICT, WITHOUT ROWID;

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant'))
) STRICT, WITHOUT ROWID;

CREATE TABLE message_revisions (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    content BLOB NOT NULL,
    UNIQUE (message_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_contexts (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    entries TEXT NOT NULL CHECK (json_type(entries) = 'array')
) STRICT, WITHOUT ROWID;

CREATE TABLE agent_sessions (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    scope TEXT NOT NULL CHECK (json_type(scope) = 'object'),
    provider_session_id TEXT,
    cursor_context_id TEXT REFERENCES conversation_contexts(id)
) STRICT, WITHOUT ROWID;

CREATE TABLE agent_attempts (
    run_id TEXT PRIMARY KEY REFERENCES runs(id),
    turn_id TEXT NOT NULL UNIQUE REFERENCES turns(id),
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    ordinal INTEGER NOT NULL,
    agent_session_id TEXT NOT NULL REFERENCES agent_sessions(id),
    previous_run_id TEXT REFERENCES agent_attempts(run_id),
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    resume_cursor_id TEXT REFERENCES conversation_contexts(id),
    provider_turn_id TEXT,
    reported_model TEXT,
    reported_effort TEXT,
    provider_terminal TEXT CHECK (provider_terminal IN ('completed', 'failed', 'interrupted')),
    UNIQUE (conversation_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE active_agent_attempts (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id),
    run_id TEXT NOT NULL UNIQUE REFERENCES agent_attempts(run_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE agent_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    payload BLOB NOT NULL
) STRICT;

CREATE INDEX agent_events_run ON agent_events(run_id, seq);
