CREATE TABLE project_tasks (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    PRIMARY KEY (project_id, id)
) STRICT, WITHOUT ROWID;
