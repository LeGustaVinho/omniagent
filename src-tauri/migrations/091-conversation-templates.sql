CREATE TABLE conversation_templates (
    project_id TEXT NOT NULL REFERENCES projects(id),
    id TEXT NOT NULL,
    head_revision TEXT NOT NULL,
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, id, head_revision)
        REFERENCES conversation_template_revisions(project_id, template_id, revision)
        DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE conversation_template_revisions (
    project_id TEXT NOT NULL,
    template_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
    base_revision TEXT,
    payload BLOB NOT NULL,
    PRIMARY KEY (project_id, template_id, revision),
    UNIQUE (project_id, template_id, ordinal),
    FOREIGN KEY (project_id, template_id) REFERENCES conversation_templates(project_id, id),
    FOREIGN KEY (project_id, template_id, base_revision)
        REFERENCES conversation_template_revisions(project_id, template_id, revision)
) STRICT, WITHOUT ROWID;
