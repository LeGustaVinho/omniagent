CREATE TABLE git_reviewed_hunks (
    checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
    comparison_snapshot TEXT NOT NULL,
    hunk_id TEXT NOT NULL,
    PRIMARY KEY(checkout_id, comparison_snapshot, hunk_id)
) STRICT, WITHOUT ROWID;
