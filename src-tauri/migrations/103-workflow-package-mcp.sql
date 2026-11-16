ALTER TABLE workflow_package_requests
    ADD COLUMN structured_mcp INTEGER NOT NULL DEFAULT 0 CHECK (structured_mcp IN (0, 1));

CREATE TABLE workflow_package_mcp_sessions (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id),
    authoring_request_id TEXT NOT NULL REFERENCES workflow_package_requests(id),
    draft_id TEXT NOT NULL REFERENCES workflow_package_drafts(id),
    base_revision TEXT NOT NULL,
    working_revision TEXT NOT NULL,
    submitted_revision TEXT,
    FOREIGN KEY (draft_id, base_revision) REFERENCES workflow_package_revisions(draft_id, id),
    FOREIGN KEY (draft_id, working_revision) REFERENCES workflow_package_revisions(draft_id, id),
    FOREIGN KEY (draft_id, submitted_revision) REFERENCES workflow_package_revisions(draft_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_package_mcp_calls (
    run_id TEXT NOT NULL REFERENCES workflow_package_mcp_sessions(run_id),
    request_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    response BLOB NOT NULL,
    PRIMARY KEY (run_id, request_id)
) STRICT, WITHOUT ROWID;
