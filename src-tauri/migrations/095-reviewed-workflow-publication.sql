CREATE TABLE workflow_published_heads (
    project_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    PRIMARY KEY (project_id, workflow_id),
    FOREIGN KEY (project_id, workflow_id, revision)
        REFERENCES workflow_revisions(project_id, workflow_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_authoring_publications (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    workflow_revision TEXT NOT NULL,
    previous_revision TEXT,
    draft_id TEXT NOT NULL UNIQUE REFERENCES workflow_authoring_drafts(id),
    reviewed_revision_id TEXT NOT NULL UNIQUE REFERENCES workflow_authoring_revisions(id),
    originating_request_id TEXT REFERENCES workflow_authoring_requests(id),
    originating_run_id TEXT REFERENCES runs(id),
    decision_source TEXT NOT NULL CHECK (decision_source = 'desktop_user'),
    confirmed_at INTEGER NOT NULL DEFAULT (unixepoch()),
    CHECK ((originating_request_id IS NULL) = (originating_run_id IS NULL)),
    FOREIGN KEY (project_id, workflow_id, workflow_revision)
        REFERENCES workflow_revisions(project_id, workflow_id, revision),
    FOREIGN KEY (project_id, workflow_id, previous_revision)
        REFERENCES workflow_revisions(project_id, workflow_id, revision)
) STRICT;

CREATE INDEX workflow_publications_by_version
    ON workflow_authoring_publications(project_id, workflow_id, workflow_revision);
