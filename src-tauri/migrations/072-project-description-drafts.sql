CREATE TABLE project_description_drafts (
    project_id TEXT NOT NULL,
    epic_id TEXT,
    task_id TEXT,
    draft_revision TEXT NOT NULL,
    base_revision_id TEXT,
    body BLOB NOT NULL,
    PRIMARY KEY (project_id, draft_revision),
    UNIQUE (project_id, epic_id),
    UNIQUE (project_id, task_id),
    FOREIGN KEY (project_id, epic_id) REFERENCES project_epics(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, base_revision_id) REFERENCES project_description_revisions(project_id, id) ON DELETE CASCADE,
    CHECK ((epic_id IS NULL) <> (task_id IS NULL))
) STRICT, WITHOUT ROWID;
