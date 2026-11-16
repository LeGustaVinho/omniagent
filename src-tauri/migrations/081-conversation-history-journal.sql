CREATE TABLE conversation_history_journal (
    sequence INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    previous_context_id TEXT NOT NULL REFERENCES conversation_contexts(id) ON DELETE CASCADE,
    next_context_id TEXT NOT NULL REFERENCES conversation_contexts(id) ON DELETE CASCADE,
    action TEXT NOT NULL CHECK (action IN ('rewind', 'clear', 'select')),
    replaced_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX conversation_history_journal_owner
    ON conversation_history_journal(conversation_id, sequence DESC);
