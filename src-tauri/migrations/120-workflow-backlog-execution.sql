CREATE TABLE workflow_backlog_executions (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_automatic_runs(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    backlog_artifact_id TEXT NOT NULL REFERENCES workflow_artifacts(id) ON DELETE CASCADE,
    plan_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending', 'completed', 'failed')),
    reason TEXT,
    destination_branch TEXT,
    next_check INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (pipeline_run_id, node_instance_id, backlog_artifact_id),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_backlog_due ON workflow_backlog_executions(next_check)
    WHERE state = 'pending';

CREATE TABLE workflow_backlog_task_runs (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    backlog_artifact_id TEXT NOT NULL,
    task_key TEXT NOT NULL,
    task_id TEXT NOT NULL,
    child_run_id TEXT NOT NULL UNIQUE REFERENCES workflow_run_pins(pipeline_run_id),
    destination_branch TEXT NOT NULL,
    PRIMARY KEY (pipeline_run_id, task_key),
    FOREIGN KEY (pipeline_run_id, task_key)
        REFERENCES workflow_backlog_tasks(pipeline_run_id, task_key) ON DELETE CASCADE,
    FOREIGN KEY (pipeline_run_id, node_instance_id, backlog_artifact_id)
        REFERENCES workflow_backlog_executions(pipeline_run_id, node_instance_id, backlog_artifact_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE workflow_child_origins (
    child_run_id TEXT PRIMARY KEY REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE,
    event_id TEXT,
    automation_chain TEXT NOT NULL CHECK(json_type(automation_chain) = 'array')
) STRICT, WITHOUT ROWID;
