CREATE TABLE drafts (
    kind TEXT NOT NULL CHECK (kind IN ('conversation', 'checkout_file')),
    owner_id TEXT NOT NULL CHECK (length(owner_id) > 0),
    document TEXT NOT NULL,
    payload BLOB NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (kind, owner_id, document),
    CHECK (
        (kind = 'conversation' AND document = '')
        OR (kind = 'checkout_file' AND length(document) > 0)
    )
) STRICT, WITHOUT ROWID;
