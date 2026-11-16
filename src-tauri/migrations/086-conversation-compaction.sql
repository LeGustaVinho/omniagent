CREATE TABLE project_conversation_compaction_settings (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    automatic INTEGER NOT NULL CHECK (automatic IN (0, 1)),
    threshold_percent INTEGER NOT NULL CHECK (threshold_percent BETWEEN 1 AND 100)
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_compaction_requests (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL UNIQUE,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    source_context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    summary_job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    coverage TEXT NOT NULL CHECK (json_type(coverage) = 'object'),
    state TEXT NOT NULL CHECK (state IN ('queued', 'activated', 'failed', 'stale')),
    detail TEXT,
    FOREIGN KEY (conversation_id, source_context_id) REFERENCES conversation_contexts(conversation_id, id)
) STRICT;

CREATE INDEX conversation_compaction_requests_by_conversation
ON conversation_compaction_requests(conversation_id, sequence DESC);

CREATE TABLE conversation_context_compactions (
    context_id TEXT PRIMARY KEY REFERENCES conversation_contexts(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL REFERENCES conversation_artifact_revisions(id) ON DELETE CASCADE,
    coverage TEXT NOT NULL CHECK (json_type(coverage) = 'object')
) STRICT, WITHOUT ROWID;
