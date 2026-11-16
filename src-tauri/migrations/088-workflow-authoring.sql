CREATE TABLE workflow_authoring_drafts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    checkout_id TEXT NOT NULL REFERENCES checkouts(id),
    conversation_id TEXT NOT NULL REFERENCES conversations(id),
    workflow_id TEXT NOT NULL,
    base_revision TEXT,
    head_revision TEXT,
    FOREIGN KEY (project_id, workflow_id, base_revision)
        REFERENCES workflow_revisions(project_id, workflow_id, revision)
) STRICT;

CREATE INDEX workflow_authoring_conversation ON workflow_authoring_drafts(conversation_id);

CREATE TABLE workflow_authoring_requests (
    id TEXT PRIMARY KEY,
    draft_id TEXT NOT NULL REFERENCES workflow_authoring_drafts(id),
    parent_revision TEXT,
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    payload BLOB NOT NULL,
    result TEXT CHECK (result IN ('saved', 'invalid', 'superseded')),
    diagnostic BLOB
) STRICT;

CREATE INDEX workflow_authoring_requests_by_draft ON workflow_authoring_requests(draft_id);

CREATE TABLE workflow_authoring_revisions (
    id TEXT PRIMARY KEY,
    draft_id TEXT NOT NULL REFERENCES workflow_authoring_drafts(id),
    parent_revision TEXT,
    request_id TEXT UNIQUE REFERENCES workflow_authoring_requests(id),
    run_id TEXT REFERENCES runs(id),
    payload BLOB NOT NULL
) STRICT;
