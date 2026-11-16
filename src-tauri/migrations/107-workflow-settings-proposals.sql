CREATE TABLE workflow_settings_timeout_revisions (
    project_id TEXT PRIMARY KEY REFERENCES project_question_timeouts(project_id),
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_template_display_labels (
    project_id TEXT NOT NULL,
    template_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    label BLOB NOT NULL,
    PRIMARY KEY (project_id, template_id),
    FOREIGN KEY (project_id, template_id) REFERENCES conversation_templates(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_settings_allowlist_revisions (
    project_id TEXT NOT NULL REFERENCES projects(id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    grants BLOB NOT NULL,
    PRIMARY KEY (project_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_settings_proposals (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    origin_run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    dispatch_id TEXT NOT NULL REFERENCES dispatches(id),
    request_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    payload BLOB NOT NULL,
    UNIQUE (origin_run_id, request_id)
) STRICT;

CREATE INDEX workflow_settings_proposals_project ON workflow_settings_proposals(project_id);
