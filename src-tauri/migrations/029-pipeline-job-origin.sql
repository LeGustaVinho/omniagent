CREATE TABLE job_pipeline_runs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    plan_revision TEXT,
    UNIQUE (id, project_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE job_pipeline_nodes (
    pipeline_run_id TEXT NOT NULL REFERENCES job_pipeline_runs(id),
    instance_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('execution', 'implementation_approval')),
    PRIMARY KEY (pipeline_run_id, instance_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE job_pipeline_releases (
    id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL,
    approval_node_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    checkout_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    plan_revision TEXT NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
    UNIQUE (pipeline_run_id, id),
    FOREIGN KEY (pipeline_run_id, project_id) REFERENCES job_pipeline_runs(id, project_id),
    FOREIGN KEY (pipeline_run_id, approval_node_id) REFERENCES job_pipeline_nodes(pipeline_run_id, instance_id),
    FOREIGN KEY (project_id, checkout_id) REFERENCES checkouts(project_id, id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id)
) STRICT, WITHOUT ROWID;

CREATE TABLE job_pipeline_origins (
    job_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_revision TEXT,
    release_id TEXT,
    FOREIGN KEY (project_id, job_id) REFERENCES jobs(project_id, id),
    FOREIGN KEY (pipeline_run_id, project_id) REFERENCES job_pipeline_runs(id, project_id),
    FOREIGN KEY (pipeline_run_id, node_instance_id) REFERENCES job_pipeline_nodes(pipeline_run_id, instance_id),
    FOREIGN KEY (pipeline_run_id, release_id) REFERENCES job_pipeline_releases(pipeline_run_id, id)
) STRICT, WITHOUT ROWID;
