CREATE TABLE conversation_attachment_drafts (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    revision TEXT NOT NULL,
    payload BLOB NOT NULL,
    preview_id TEXT,
    preview_payload BLOB,
    CHECK ((preview_id IS NULL) = (preview_payload IS NULL))
) STRICT, WITHOUT ROWID;
