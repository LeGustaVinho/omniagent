CREATE TABLE saved_attachment_reuses (
    copied_revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    copied_attachment_ordinal INTEGER NOT NULL CHECK (copied_attachment_ordinal >= 0),
    source_revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    source_attachment_ordinal INTEGER NOT NULL CHECK (source_attachment_ordinal >= 0),
    copied_identity BLOB NOT NULL,
    PRIMARY KEY (copied_revision_id, copied_attachment_ordinal),
    CHECK (copied_revision_id <> source_revision_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX saved_attachment_reuses_by_source
    ON saved_attachment_reuses(source_revision_id, source_attachment_ordinal);
