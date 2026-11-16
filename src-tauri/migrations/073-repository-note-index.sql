CREATE TABLE repository_note_index (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    title TEXT NOT NULL,
    search_text TEXT NOT NULL,
    metadata TEXT NOT NULL CHECK (json_valid(metadata)),
    UNIQUE (checkout_id, path)
) STRICT;

CREATE VIRTUAL TABLE repository_note_fts USING fts5(
    title, search_text, path,
    content='repository_note_index', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER repository_note_fts_insert AFTER INSERT ON repository_note_index BEGIN
    INSERT INTO repository_note_fts(rowid, title, search_text, path)
    VALUES (new.id, new.title, new.search_text, new.path);
END;

CREATE TRIGGER repository_note_fts_delete AFTER DELETE ON repository_note_index BEGIN
    INSERT INTO repository_note_fts(repository_note_fts, rowid, title, search_text, path)
    VALUES ('delete', old.id, old.title, old.search_text, old.path);
END;

CREATE TRIGGER repository_note_fts_update AFTER UPDATE OF title, search_text, path ON repository_note_index BEGIN
    INSERT INTO repository_note_fts(repository_note_fts, rowid, title, search_text, path)
    VALUES ('delete', old.id, old.title, old.search_text, old.path);
    INSERT INTO repository_note_fts(rowid, title, search_text, path)
    VALUES (new.id, new.title, new.search_text, new.path);
END;
