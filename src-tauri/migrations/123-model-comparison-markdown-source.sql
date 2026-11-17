CREATE TABLE model_comparison_markdown_sources (
    group_id TEXT PRIMARY KEY REFERENCES model_comparison_groups(id) ON DELETE CASCADE,
    checkout_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    path TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    FOREIGN KEY (checkout_id, revision_id) REFERENCES repository_markdown_revisions(checkout_id, id)
) STRICT, WITHOUT ROWID;
