CREATE TABLE project_fork_checkout_policies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    choice TEXT NOT NULL CHECK(choice IN ('reuse_current_checkout', 'create_separate_worktree', 'ask_for_checkout'))
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_fork_checkout_policies (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    choice TEXT NOT NULL CHECK(choice IN ('reuse_current_checkout', 'create_separate_worktree', 'ask_for_checkout'))
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_fork_checkout_previews (
    id TEXT PRIMARY KEY,
    source_conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    source_context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    payload BLOB NOT NULL,
    fork_conversation_id TEXT UNIQUE REFERENCES conversations(id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX conversation_fork_checkout_previews_source ON conversation_fork_checkout_previews(source_conversation_id);
