CREATE TABLE message_attachments (
    message_revision_id TEXT PRIMARY KEY REFERENCES message_revisions(id) ON DELETE CASCADE,
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;
