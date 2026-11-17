CREATE TABLE workflow_github_project_settings (
    project_id TEXT PRIMARY KEY NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    revision TEXT NOT NULL,
    payload BLOB NOT NULL
) STRICT;

CREATE TABLE workflow_github_delivery_pins (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE,
    plan_key TEXT NOT NULL,
    policy_revision TEXT NOT NULL,
    destination_branch TEXT NOT NULL,
    PRIMARY KEY (pipeline_run_id, plan_key)
) STRICT;

CREATE TABLE workflow_github_actions (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    request_key TEXT NOT NULL,
    inputs BLOB NOT NULL,
    port TEXT CHECK (port IN ('completed', 'passed', 'approval_required', 'merged', 'failed', 'stale')),
    output_id TEXT,
    remote_request_id TEXT,
    remote_proof BLOB,
    reason TEXT,
    retry_after INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (pipeline_run_id, node_instance_id, unit_key, plan_key, request_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id) REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX workflow_github_actions_ready ON workflow_github_actions(retry_after)
    WHERE port IS NULL;

CREATE TABLE workflow_github_branches (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    repository_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    task_id TEXT NOT NULL,
    previous_head TEXT,
    target_head TEXT NOT NULL,
    published INTEGER NOT NULL CHECK (published IN (0, 1)),
    PRIMARY KEY (project_id, repository_id, branch)
) STRICT;

CREATE TABLE workflow_github_destinations (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    repository_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    observed_tip TEXT NOT NULL,
    PRIMARY KEY (project_id, repository_id, branch)
) STRICT;
