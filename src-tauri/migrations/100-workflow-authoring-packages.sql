CREATE TABLE workflow_package_drafts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    conversation_id TEXT REFERENCES conversations(id),
    head_revision TEXT,
    FOREIGN KEY (id, head_revision)
        REFERENCES workflow_package_revisions(draft_id, id)
) STRICT;

CREATE TABLE workflow_package_revisions (
    draft_id TEXT NOT NULL REFERENCES workflow_package_drafts(id),
    id TEXT NOT NULL,
    parent_revision TEXT,
    run_id TEXT REFERENCES runs(id),
    payload BLOB NOT NULL,
    PRIMARY KEY (draft_id, id),
    FOREIGN KEY (draft_id, parent_revision)
        REFERENCES workflow_package_revisions(draft_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_package_requests (
    id TEXT PRIMARY KEY,
    draft_id TEXT NOT NULL REFERENCES workflow_package_drafts(id),
    parent_revision TEXT NOT NULL,
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    payload BLOB NOT NULL,
    result TEXT CHECK (result IN ('saved', 'invalid', 'superseded')),
    diagnostic BLOB,
    FOREIGN KEY (draft_id, parent_revision)
        REFERENCES workflow_package_revisions(draft_id, id)
) STRICT;

CREATE TABLE workflow_package_artifact_revisions (
    scope_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('pipeline', 'role', 'preset', 'subflow', 'command', 'mcp_tool')),
    artifact_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (scope_key, kind, artifact_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_package_artifact_heads (
    scope_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    PRIMARY KEY (scope_key, kind, artifact_id),
    FOREIGN KEY (scope_key, kind, artifact_id, revision)
        REFERENCES workflow_package_artifact_revisions(scope_key, kind, artifact_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_package_publications (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    draft_id TEXT NOT NULL UNIQUE REFERENCES workflow_package_drafts(id),
    reviewed_revision TEXT NOT NULL,
    originating_request_id TEXT REFERENCES workflow_package_requests(id),
    originating_run_id TEXT REFERENCES runs(id),
    decision_source TEXT NOT NULL CHECK (decision_source = 'desktop_user'),
    confirmed_at INTEGER NOT NULL DEFAULT (unixepoch()),
    payload BLOB NOT NULL,
    CHECK ((originating_request_id IS NULL) = (originating_run_id IS NULL)),
    FOREIGN KEY (draft_id, reviewed_revision)
        REFERENCES workflow_package_revisions(draft_id, id)
) STRICT;
