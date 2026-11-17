CREATE TABLE IF NOT EXISTS conversation_pipeline_turns (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    payload BLOB NOT NULL,
    message_revision_id TEXT,
    history_context_id TEXT,
    workflow_run_id TEXT,
    state TEXT NOT NULL DEFAULT 'queued',
    result_revision_id TEXT,
    error TEXT,
    UNIQUE (conversation_id, request_id)
) STRICT;

CREATE INDEX IF NOT EXISTS conversation_pipeline_turns_by_conversation
    ON conversation_pipeline_turns(conversation_id, sequence);

CREATE INDEX IF NOT EXISTS conversation_pipeline_turns_by_state
    ON conversation_pipeline_turns(state, sequence);

CREATE UNIQUE INDEX IF NOT EXISTS conversation_pipeline_turns_by_run
    ON conversation_pipeline_turns(workflow_run_id) WHERE workflow_run_id IS NOT NULL;
