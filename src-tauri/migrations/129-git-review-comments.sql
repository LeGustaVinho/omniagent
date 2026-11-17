CREATE TABLE git_review_comments (
    id TEXT PRIMARY KEY,
    checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
    surface TEXT NOT NULL CHECK (surface IN ('working', 'staged', 'branch', 'versions')),
    path TEXT NOT NULL,
    payload BLOB NOT NULL,
    resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT, WITHOUT ROWID;

CREATE INDEX git_review_comments_scope ON git_review_comments(checkout_id, surface, created_at DESC, id);
