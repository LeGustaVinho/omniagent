ALTER TABLE workflow_trigger_activations RENAME TO previous_workflow_trigger_activations;
ALTER TABLE workflow_trigger_revisions RENAME TO previous_workflow_trigger_revisions;

CREATE TABLE workflow_trigger_revisions (
    project_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('created', 'state_changed', 'checklist_checked', 'checklist_reopened')),
    work_kind TEXT NOT NULL CHECK (work_kind IN ('all', 'task', 'subtask')),
    target_state TEXT,
    workflow_id TEXT NOT NULL,
    workflow_revision TEXT NOT NULL,
    checkout_id TEXT NOT NULL,
    markdown_revision_id TEXT,
    PRIMARY KEY (project_id, rule_id, revision),
    FOREIGN KEY (project_id, rule_id) REFERENCES workflow_trigger_rules(project_id, id) ON DELETE CASCADE,
    FOREIGN KEY (project_id, workflow_id, workflow_revision) REFERENCES workflow_revisions(project_id, workflow_id, revision),
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id),
    FOREIGN KEY (checkout_id, markdown_revision_id) REFERENCES repository_markdown_revisions(checkout_id, id),
    CHECK (event_kind = 'state_changed' OR target_state IS NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_trigger_activations (
    project_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    event_id TEXT NOT NULL REFERENCES project_work_events(id),
    rule_revision INTEGER NOT NULL,
    pipeline_run_id TEXT UNIQUE REFERENCES job_pipeline_runs(id),
    outcome TEXT NOT NULL CHECK (outcome IN ('launched', 'skipped_cycle', 'failed')),
    detail TEXT,
    PRIMARY KEY (project_id, rule_id, event_id),
    FOREIGN KEY (project_id, rule_id, rule_revision) REFERENCES workflow_trigger_revisions(project_id, rule_id, revision),
    CHECK ((outcome = 'launched') = (pipeline_run_id IS NOT NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO workflow_trigger_revisions SELECT * FROM previous_workflow_trigger_revisions;
INSERT INTO workflow_trigger_activations SELECT * FROM previous_workflow_trigger_activations;
DROP TABLE previous_workflow_trigger_activations;
DROP TABLE previous_workflow_trigger_revisions;
