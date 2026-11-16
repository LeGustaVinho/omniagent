-- Run with foreign keys disabled and check all references before committing.
CREATE TABLE workflow_artifacts_with_iterations (
    id TEXT PRIMARY KEY,
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    port_name TEXT NOT NULL,
    unit_id TEXT REFERENCES workflow_delivery_units(unit_id),
    kind TEXT NOT NULL CHECK (kind IN ('markdown', 'plan', 'review', 'task', 'approval', 'release',
                                     'worktree', 'candidate', 'checks', 'acceptance', 'git',
                                     'json_list', 'json_item', 'iteration_results')),
    payload BLOB NOT NULL,
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id)
) STRICT, WITHOUT ROWID;

INSERT INTO workflow_artifacts_with_iterations
SELECT id, pipeline_run_id, node_instance_id, port_name, unit_id, kind, payload FROM workflow_artifacts;
DROP TABLE workflow_artifacts;
ALTER TABLE workflow_artifacts_with_iterations RENAME TO workflow_artifacts;
