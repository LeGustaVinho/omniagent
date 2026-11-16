CREATE TABLE workflow_revisions (
    project_id TEXT NOT NULL REFERENCES projects(id),
    workflow_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    definition BLOB NOT NULL,
    PRIMARY KEY (project_id, workflow_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_run_pins (
    pipeline_run_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    workflow_revision TEXT NOT NULL,
    FOREIGN KEY (pipeline_run_id, project_id) REFERENCES job_pipeline_runs(id, project_id),
    FOREIGN KEY (project_id, workflow_id, workflow_revision)
        REFERENCES workflow_revisions(project_id, workflow_id, revision)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_node_pins (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    node_instance_id TEXT NOT NULL,
    instance_revision TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    scheduler_profile TEXT CHECK (scheduler_profile IN ('planning', 'read_only', 'approved_write')),
    requires_plan INTEGER NOT NULL CHECK (requires_plan IN (0, 1)),
    PRIMARY KEY (pipeline_run_id, node_instance_id),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES job_pipeline_nodes(pipeline_run_id, instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_artifacts (
    id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    port_name TEXT NOT NULL,
    unit_id TEXT REFERENCES workflow_delivery_units(unit_id),
    kind TEXT NOT NULL CHECK (kind IN ('markdown', 'plan', 'review', 'task', 'approval', 'release',
                                     'worktree', 'candidate', 'checks', 'acceptance', 'git')),
    payload BLOB NOT NULL,
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_host_completions (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    execution_key TEXT NOT NULL,
    request_revision TEXT NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES workflow_artifacts(id),
    PRIMARY KEY (pipeline_run_id, node_instance_id, execution_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_approval_decisions (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_revision TEXT NOT NULL REFERENCES workflow_artifacts(id),
    decision_artifact_id TEXT NOT NULL UNIQUE REFERENCES workflow_artifacts(id),
    PRIMARY KEY (pipeline_run_id, node_instance_id, plan_revision),
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

CREATE TABLE workflow_delivery_units (
    unit_id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id),
    plan_revision TEXT NOT NULL REFERENCES workflow_artifacts(id),
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    checkout_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    unit BLOB NOT NULL,
    commit_evidence_id TEXT,
    UNIQUE (pipeline_run_id, plan_revision, ordinal),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id),
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_job_admissions (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    PRIMARY KEY (pipeline_run_id, node_instance_id),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_job_inputs (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    unit_id TEXT REFERENCES workflow_delivery_units(unit_id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TRIGGER workflow_plan_must_be_published BEFORE UPDATE OF plan_revision ON job_pipeline_runs
WHEN NEW.plan_revision IS NOT NULL AND EXISTS (
    SELECT 1 FROM workflow_run_pins p WHERE p.pipeline_run_id = OLD.id
)
AND NOT EXISTS (
    SELECT 1 FROM workflow_artifacts a WHERE a.id = NEW.plan_revision
        AND a.pipeline_run_id = OLD.id AND a.kind = 'plan'
)
BEGIN
    SELECT RAISE(ABORT, 'workflow plan does not belong to this pipeline run');
END;

CREATE TRIGGER workflow_job_must_match_pin BEFORE INSERT ON job_pipeline_origins
WHEN EXISTS (
    SELECT 1 FROM workflow_run_pins p WHERE p.pipeline_run_id = NEW.pipeline_run_id
)
BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM workflow_job_admissions a
        WHERE a.pipeline_run_id = NEW.pipeline_run_id AND a.node_instance_id = NEW.node_instance_id
    ) AND NOT EXISTS (
        SELECT 1 FROM jobs child JOIN job_attempts a ON a.run_id = child.origin_run_id
        JOIN job_pipeline_origins parent ON parent.job_id = a.job_id
        WHERE child.id = NEW.job_id AND parent.pipeline_run_id = NEW.pipeline_run_id
          AND parent.node_instance_id = NEW.node_instance_id AND parent.plan_revision IS NEW.plan_revision
          AND parent.release_id IS NEW.release_id
    ) THEN RAISE(ABORT, 'typed workflow jobs require verified inputs') END;
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM workflow_node_pins n JOIN jobs j ON j.id = NEW.job_id
        WHERE n.pipeline_run_id = NEW.pipeline_run_id AND n.node_instance_id = NEW.node_instance_id
            AND (n.scheduler_profile = j.mode
                OR (n.scheduler_profile = 'planning' AND j.mode IN ('plan', 'read_only'))
                OR (j.origin_run_id IS NOT NULL AND j.mode = 'read_only'))
            AND (n.requires_plan = 0 OR NEW.plan_revision IS NOT NULL)
    ) THEN RAISE(ABORT, 'job does not match its pinned workflow node and profile') END;
    SELECT CASE WHEN NEW.plan_revision IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM workflow_artifacts a WHERE a.id = NEW.plan_revision
            AND a.pipeline_run_id = NEW.pipeline_run_id AND a.kind = 'plan'
    ) THEN RAISE(ABORT, 'job plan does not belong to its pinned workflow') END;
END;
