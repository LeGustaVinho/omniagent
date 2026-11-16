CREATE TABLE dispatch_lineage (
    dispatch_id TEXT PRIMARY KEY REFERENCES dispatches(id) ON DELETE CASCADE,
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

-- Index the application's opaque message revision IDs only. File metadata,
-- paths, content hashes and selected ranges remain inside the protected payload.
CREATE TABLE dispatch_lineage_messages (
    dispatch_id TEXT NOT NULL REFERENCES dispatch_lineage(dispatch_id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL,
    PRIMARY KEY (dispatch_id, revision_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX dispatch_lineage_messages_by_revision ON dispatch_lineage_messages(revision_id, dispatch_id);

-- A fork clones saved message and attachment ciphertext. A revision edge
-- preserves that origin even after either conversation's current context changes.
CREATE TABLE message_revision_copy_sources (
    copied_revision_id TEXT PRIMARY KEY,
    source_revision_id TEXT NOT NULL,
    CHECK (copied_revision_id <> source_revision_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX message_revision_copy_sources_by_source ON message_revision_copy_sources(source_revision_id, copied_revision_id);

-- Recover legacy fork edges only for a unique frozen ciphertext match within
-- the exact original context prefix, never by title, text, or current version.
INSERT INTO message_revision_copy_sources (copied_revision_id, source_revision_id)
SELECT copied.id, min(source.id)
FROM conversation_forks fork
JOIN messages copied_message ON copied_message.conversation_id = fork.conversation_id
JOIN message_revisions copied ON copied.message_id = copied_message.id AND copied.revision = 1
JOIN conversation_contexts context ON context.id = fork.source_context_id
    AND context.conversation_id = fork.source_conversation_id
JOIN json_each(context.entries) prefix
JOIN json_each(context.entries) boundary ON boundary.value = fork.source_revision_id AND prefix.key <= boundary.key
JOIN message_revisions source ON source.id = prefix.value AND source.content = copied.content
JOIN messages source_message ON source_message.id = source.message_id
    AND source_message.conversation_id = fork.source_conversation_id AND source_message.role = copied_message.role
WHERE copied.id <> source.id
GROUP BY copied.id
HAVING count(DISTINCT source.id) = 1;

CREATE TABLE conversation_summary_requests (
    id TEXT PRIMARY KEY,
    source_conversation_id TEXT NOT NULL REFERENCES conversations(id),
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_artifact_revisions (
    id TEXT PRIMARY KEY,
    source_conversation_id TEXT NOT NULL REFERENCES conversations(id),
    summary_run_id TEXT UNIQUE REFERENCES agent_attempts(run_id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_artifact_sources (
    artifact_revision_id TEXT NOT NULL REFERENCES conversation_artifact_revisions(id) ON DELETE CASCADE,
    message_revision_id TEXT NOT NULL,
    PRIMARY KEY (artifact_revision_id, message_revision_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX conversation_artifact_sources_by_revision ON conversation_artifact_sources(message_revision_id, artifact_revision_id);
