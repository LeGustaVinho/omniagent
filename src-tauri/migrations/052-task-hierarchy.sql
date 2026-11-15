CREATE TABLE project_epics (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    PRIMARY KEY (project_id, id)
) STRICT, WITHOUT ROWID;

-- Existing task IDs remain the work scope for both tasks and sub-tasks.
CREATE TABLE project_task_memberships (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    epic_id TEXT,
    parent_task_id TEXT,
    PRIMARY KEY (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, epic_id) REFERENCES project_epics(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, parent_task_id) REFERENCES project_tasks(project_id, id),
    CHECK ((epic_id IS NULL) <> (parent_task_id IS NULL)),
    CHECK (task_id <> parent_task_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX project_memberships_by_parent
    ON project_task_memberships(project_id, parent_task_id);
CREATE INDEX project_memberships_by_epic
    ON project_task_memberships(project_id, epic_id);

CREATE TRIGGER project_task_membership_insert_depth
BEFORE INSERT ON project_task_memberships WHEN NEW.parent_task_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a sub-task cannot parent another sub-task')
    WHERE EXISTS (
        SELECT 1 FROM project_task_memberships
        WHERE project_id = NEW.project_id AND (
            (task_id = NEW.parent_task_id AND parent_task_id IS NOT NULL)
            OR parent_task_id = NEW.task_id
        )
    );
END;

CREATE TRIGGER project_task_membership_update_depth
BEFORE UPDATE ON project_task_memberships WHEN NEW.parent_task_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a sub-task cannot parent another sub-task')
    WHERE EXISTS (
        SELECT 1 FROM project_task_memberships
        WHERE project_id = NEW.project_id AND (
            (task_id = NEW.parent_task_id AND parent_task_id IS NOT NULL)
            OR parent_task_id = NEW.task_id
        )
    );
END;

CREATE TABLE project_checklists (
    project_id TEXT NOT NULL,
    id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
