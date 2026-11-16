CREATE TABLE project_context_records (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN (
        'project_convention', 'architecture_decision', 'task_criterion', 'code_reference', 'note', 'prior_run_evidence'
    )),
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE,
    CHECK (kind <> 'task_criterion' OR task_id IS NOT NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE project_context_revisions (
    project_id TEXT NOT NULL,
    record_id TEXT NOT NULL,
    id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    status TEXT NOT NULL CHECK (status IN ('proposed', 'in_review', 'accepted', 'rejected', 'superseded')),
    content BLOB NOT NULL,
    origin_run_id TEXT REFERENCES runs(id),
    markdown_checkout_id TEXT,
    markdown_revision_id TEXT,
    PRIMARY KEY (project_id, record_id, id),
    UNIQUE (project_id, record_id, revision),
    FOREIGN KEY (project_id, record_id) REFERENCES project_context_records(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, markdown_checkout_id) REFERENCES checkouts(project_id, id),
    FOREIGN KEY (markdown_checkout_id, markdown_revision_id) REFERENCES repository_markdown_revisions(checkout_id, id),
    CHECK ((markdown_checkout_id IS NULL) = (markdown_revision_id IS NULL)),
    CHECK (origin_run_id IS NULL OR markdown_revision_id IS NULL)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX project_context_active_version
    ON project_context_revisions(project_id, record_id) WHERE status = 'accepted';
CREATE UNIQUE INDEX project_context_open_proposal
    ON project_context_revisions(project_id, record_id) WHERE status IN ('proposed', 'in_review');

CREATE TABLE project_context_review_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    record_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proposed', 'in_review', 'accepted', 'rejected', 'superseded')),
    related_revision_id TEXT,
    recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (project_id, record_id, revision_id) REFERENCES project_context_revisions(project_id, record_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, record_id, related_revision_id) REFERENCES project_context_revisions(project_id, record_id, id) DEFERRABLE INITIALLY DEFERRED,
    CHECK ((status = 'superseded') = (related_revision_id IS NOT NULL)),
    CHECK (revision_id <> related_revision_id)
) STRICT;

CREATE INDEX project_context_events_by_record ON project_context_review_events(project_id, record_id, sequence);
