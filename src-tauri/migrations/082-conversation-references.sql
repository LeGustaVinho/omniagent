CREATE TABLE conversation_reference_previews (
    id TEXT PRIMARY KEY,
    destination_conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    source_conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL REFERENCES projects(id),
    artifact_id TEXT NOT NULL REFERENCES conversation_artifact_revisions(id) ON DELETE CASCADE,
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX conversation_reference_previews_destination
    ON conversation_reference_previews(destination_conversation_id);

CREATE TABLE conversation_reference_drafts (
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    preview_id TEXT NOT NULL REFERENCES conversation_reference_previews(id) ON DELETE CASCADE,
    PRIMARY KEY (conversation_id, ordinal),
    UNIQUE (conversation_id, preview_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE message_conversation_references (
    message_revision_id TEXT NOT NULL REFERENCES message_revisions(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    preview_id TEXT NOT NULL REFERENCES conversation_reference_previews(id) ON DELETE CASCADE,
    PRIMARY KEY (message_revision_id, ordinal)
) STRICT, WITHOUT ROWID;
