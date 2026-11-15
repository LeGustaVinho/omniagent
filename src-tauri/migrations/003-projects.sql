CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    git_common_dir TEXT NOT NULL UNIQUE,
    root TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE checkouts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    git_dir TEXT NOT NULL,
    root TEXT NOT NULL,
    UNIQUE (project_id, git_dir)
) STRICT, WITHOUT ROWID;
