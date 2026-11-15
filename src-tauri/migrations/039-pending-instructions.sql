CREATE TABLE queued_conversation_instructions (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL UNIQUE REFERENCES messages(id),
    revision_id TEXT NOT NULL UNIQUE REFERENCES message_revisions(id),
    attachment_labels BLOB
) STRICT, WITHOUT ROWID;

CREATE INDEX jobs_by_conversation_sequence ON jobs(conversation_id, sequence);
