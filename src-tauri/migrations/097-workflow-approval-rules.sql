CREATE TABLE workflow_approval_rule_revisions (
    revision TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    workflow_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    settings BLOB NOT NULL,
    UNIQUE (project_id, workflow_id, node_instance_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_approval_rule_heads (
    project_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    PRIMARY KEY (project_id, workflow_id, node_instance_id),
    FOREIGN KEY (project_id, workflow_id, node_instance_id, revision)
        REFERENCES workflow_approval_rule_revisions(project_id, workflow_id, node_instance_id, revision)
) STRICT, WITHOUT ROWID;

ALTER TABLE workflow_delivery_heads RENAME TO previous_workflow_delivery_heads;
ALTER TABLE workflow_release_bindings RENAME TO previous_workflow_release_bindings;
ALTER TABLE workflow_approval_decisions RENAME TO previous_workflow_approval_decisions;

CREATE TABLE workflow_approval_decisions (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_revision TEXT NOT NULL REFERENCES workflow_artifacts(id),
    decision_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_artifacts(id),
    generation INTEGER NOT NULL CHECK (generation > 0),
    rule_revision TEXT REFERENCES workflow_approval_rule_revisions(revision),
    PRIMARY KEY (pipeline_run_id, node_instance_id, plan_revision, generation),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_delivery_heads (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    plan_revision TEXT NOT NULL REFERENCES workflow_artifacts(id),
    decision_artifact_id TEXT NOT NULL REFERENCES workflow_approval_decisions(decision_artifact_id),
    PRIMARY KEY (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_release_bindings (
    release_id TEXT PRIMARY KEY REFERENCES job_pipeline_releases(id),
    decision_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_approval_decisions(decision_artifact_id),
    release_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_artifacts(id)
) STRICT, WITHOUT ROWID;

INSERT INTO workflow_approval_decisions SELECT *, 1, NULL FROM previous_workflow_approval_decisions;
INSERT INTO workflow_delivery_heads SELECT * FROM previous_workflow_delivery_heads;
INSERT INTO workflow_release_bindings SELECT * FROM previous_workflow_release_bindings;

DROP TABLE previous_workflow_release_bindings;
DROP TABLE previous_workflow_delivery_heads;
DROP TABLE previous_workflow_approval_decisions;

CREATE INDEX workflow_approval_decisions_rule ON workflow_approval_decisions(rule_revision);
