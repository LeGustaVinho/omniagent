CREATE TABLE repository_note_link_resolutions (
    source_id INTEGER NOT NULL REFERENCES repository_note_index(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    target_id INTEGER REFERENCES repository_note_index(id) ON DELETE SET NULL,
    resolution TEXT NOT NULL CHECK (json_valid(resolution)),
    PRIMARY KEY (source_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE INDEX repository_note_link_incoming ON repository_note_link_resolutions(target_id)
    WHERE target_id IS NOT NULL;

CREATE TABLE checkout_note_links_indexed (
    checkout_id TEXT PRIMARY KEY REFERENCES checkouts(id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
