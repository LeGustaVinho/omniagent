CREATE TABLE workflow_wait_deadlines (
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('wait', 'human')),
    deadline_at INTEGER,
    state TEXT NOT NULL DEFAULT 'waiting' CHECK (state IN ('waiting', 'resolved', 'cancelled')),
    CHECK (purpose = 'human' OR deadline_at IS NOT NULL),
    PRIMARY KEY (pipeline_run_id, node_instance_id, plan_key, unit_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX workflow_wait_deadlines_due ON workflow_wait_deadlines(deadline_at, pipeline_run_id)
WHERE state = 'waiting' AND deadline_at IS NOT NULL;
