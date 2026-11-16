CREATE TABLE repository_markdown_revisions (
    checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    path TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    base_commit TEXT,
    body BLOB NOT NULL,
    captured_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (checkout_id, id)
) STRICT, WITHOUT ROWID;

CREATE INDEX repository_markdown_recent ON repository_markdown_revisions(checkout_id, captured_at DESC);

CREATE TABLE job_repository_markdown (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    checkout_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    FOREIGN KEY (checkout_id, revision_id) REFERENCES repository_markdown_revisions(checkout_id, id)
) STRICT, WITHOUT ROWID;
