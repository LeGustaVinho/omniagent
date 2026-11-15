CREATE TABLE conversation_forks (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    source_conversation_id TEXT REFERENCES conversations(id) ON DELETE SET NULL,
    source_context_id TEXT REFERENCES conversation_contexts(id) ON DELETE SET NULL,
    source_revision_id TEXT REFERENCES message_revisions(id) ON DELETE SET NULL,
    source_session_id TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL
) STRICT, WITHOUT ROWID;
