CREATE TABLE markdown_rename_previews (
    id TEXT PRIMARY KEY,
    checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
    payload BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'applying', 'settled')),
    report BLOB,
    CHECK ((state = 'settled') = (report IS NOT NULL))
) STRICT, WITHOUT ROWID;
