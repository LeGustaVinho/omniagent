-- Run with foreign keys disabled and check all references before committing.
CREATE TABLE workflow_artifacts_with_visual_nodes (
    id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    port_name TEXT NOT NULL,
    unit_id TEXT REFERENCES workflow_delivery_units(unit_id),
    kind TEXT NOT NULL CHECK (kind IN ('markdown', 'plan', 'review', 'task', 'approval', 'release',
                                     'worktree', 'candidate', 'checks', 'acceptance', 'git',
                                     'json_list', 'json_item', 'iteration_results', 'json_object', 'rendered_text',
                                     'comparison_results', 'agile_target', 'agile_progress_receipt',
                                     'agent_response', 'agile_event', 'result_summary', 'workflow_context')),
    payload BLOB NOT NULL,
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

INSERT INTO workflow_artifacts_with_visual_nodes
SELECT id, pipeline_run_id, node_instance_id, port_name, unit_id, kind, payload FROM workflow_artifacts;
DROP TABLE workflow_artifacts;
ALTER TABLE workflow_artifacts_with_visual_nodes RENAME TO workflow_artifacts;

CREATE TABLE workflow_stage_decisions (
    request_id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    resolution BLOB,
    artifact_id TEXT REFERENCES workflow_artifacts(id),
    CHECK ((resolution IS NULL) = (artifact_id IS NULL)),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX workflow_stage_decisions_by_run ON workflow_stage_decisions(
    pipeline_run_id, plan_key, node_instance_id, unit_key
);

CREATE TABLE workflow_comparison_stages (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    execution_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    group_id TEXT UNIQUE REFERENCES model_comparison_groups(id),
    result_artifact_id TEXT REFERENCES workflow_artifacts(id),
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'settled', 'failed')),
    error TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, execution_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX workflow_comparison_stages_by_state ON workflow_comparison_stages(state);

CREATE TABLE workflow_agile_stage_actions (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    request_revision TEXT NOT NULL,
    inputs BLOB NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','completed','failed')),
    outcome BLOB,
    PRIMARY KEY(pipeline_run_id,node_instance_id,plan_key,unit_key),
    FOREIGN KEY(pipeline_run_id,node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id,node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
CREATE INDEX workflow_agile_stage_actions_pending ON workflow_agile_stage_actions(pipeline_run_id)
    WHERE state = 'pending';
