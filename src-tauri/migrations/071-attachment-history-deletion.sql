CREATE TABLE attachment_revision_copy_sources (
    copied_revision_id TEXT PRIMARY KEY REFERENCES message_revisions(id),
    source_revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    CHECK (copied_revision_id <> source_revision_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX attachment_revision_copy_sources_by_source
    ON attachment_revision_copy_sources(source_revision_id, copied_revision_id);

CREATE VIEW attachment_revision_copy_candidates AS
SELECT saved.message_revision_id AS copied_revision_id,
       coalesce(fork.source_revision_id, (
           SELECT previous.id FROM message_revisions previous
           JOIN message_attachments prior ON prior.message_revision_id = previous.id
           WHERE previous.message_id = copied.message_id AND previous.revision < copied.revision
             AND prior.payload = saved.payload
           ORDER BY previous.revision DESC LIMIT 1
       )) AS source_revision_id
FROM message_attachments saved
JOIN message_revisions copied ON copied.id = saved.message_revision_id
LEFT JOIN message_revision_copy_sources fork ON fork.copied_revision_id = copied.id
    AND EXISTS (SELECT 1 FROM message_attachments original
                WHERE original.message_revision_id = fork.source_revision_id
                  AND original.payload = saved.payload);

INSERT INTO attachment_revision_copy_sources(copied_revision_id, source_revision_id)
SELECT copied_revision_id, source_revision_id FROM attachment_revision_copy_candidates
WHERE source_revision_id IS NOT NULL;

CREATE TRIGGER remember_attachment_revision_copy AFTER INSERT ON message_attachments BEGIN
    INSERT INTO attachment_revision_copy_sources(copied_revision_id, source_revision_id)
    SELECT copied_revision_id, source_revision_id FROM attachment_revision_copy_candidates
    WHERE copied_revision_id = NEW.message_revision_id AND source_revision_id IS NOT NULL
    ON CONFLICT (copied_revision_id) DO NOTHING;
END;

CREATE TABLE removed_history_attachments (
    message_revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    attachment_ordinal INTEGER NOT NULL CHECK (attachment_ordinal >= 0),
    identity BLOB NOT NULL,
    PRIMARY KEY (message_revision_id, attachment_ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE removed_dispatch_attachments (
    dispatch_id TEXT NOT NULL REFERENCES dispatches(id),
    attachment_ordinal INTEGER NOT NULL CHECK (attachment_ordinal >= 0),
    message_revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    PRIMARY KEY (dispatch_id, attachment_ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE history_revoked_sessions (
    session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id)
) STRICT, WITHOUT ROWID;
