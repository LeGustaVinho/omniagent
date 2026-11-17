CREATE TABLE checkout_document_history (
    position INTEGER PRIMARY KEY,
    checkout_id TEXT NOT NULL,
    path TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('captured', 'before_save', 'saved')),
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (checkout_id, revision_id) REFERENCES repository_markdown_revisions(checkout_id, id) ON DELETE CASCADE
) STRICT;

INSERT INTO checkout_document_history (checkout_id, path, revision_id, origin, recorded_at)
SELECT checkout_id, path, id, 'captured', captured_at
FROM repository_markdown_revisions
WHERE substr(id, 1, 7) IN ('editor-', 'source-')
ORDER BY captured_at, checkout_id, path, id;

CREATE INDEX checkout_document_history_by_path ON checkout_document_history(checkout_id, path, position DESC);
CREATE INDEX checkout_document_history_by_revision ON checkout_document_history(checkout_id, revision_id);
