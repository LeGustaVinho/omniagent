CREATE TABLE project_description_revisions (
    project_id TEXT NOT NULL,
    id TEXT NOT NULL,
    epic_id TEXT,
    task_id TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    body BLOB,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, epic_id, revision),
    UNIQUE (project_id, task_id, revision),
    FOREIGN KEY (project_id, epic_id) REFERENCES project_epics(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE,
    CHECK ((epic_id IS NULL) <> (task_id IS NULL))
) STRICT, WITHOUT ROWID;
