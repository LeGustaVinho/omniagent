CREATE TABLE conversation_library_sections (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    name_key TEXT NOT NULL,
    UNIQUE (project_id, name_key)
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_library_organization (
    conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
    section_id TEXT REFERENCES conversation_library_sections(id) ON DELETE SET NULL,
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    unread INTEGER NOT NULL DEFAULT 0 CHECK (unread IN (0, 1)),
    archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))
) STRICT, WITHOUT ROWID;
