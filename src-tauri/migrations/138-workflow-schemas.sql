-- Extend the artifact kinds while preserving composite foreign keys from artifact heads.
CREATE TABLE workflow_package_artifact_revisions_with_schemas (
    scope_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('pipeline', 'role', 'preset', 'subflow', 'command', 'mcp_tool', 'schema')),
    artifact_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (scope_key, kind, artifact_id, revision)
) STRICT, WITHOUT ROWID;

INSERT INTO workflow_package_artifact_revisions_with_schemas
SELECT scope_key, kind, artifact_id, revision, payload FROM workflow_package_artifact_revisions;
DROP TABLE workflow_package_artifact_revisions;
ALTER TABLE workflow_package_artifact_revisions_with_schemas RENAME TO workflow_package_artifact_revisions;

-- Typed agents schedule from graph readiness on manual runs as well as automatic launches.
CREATE TABLE workflow_automatic_agents_by_run (
    pipeline_run_id TEXT NOT NULL REFERENCES workflow_run_pins(pipeline_run_id) ON DELETE CASCADE,
    node_instance_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    plan_key TEXT NOT NULL,
    payload BLOB NOT NULL,
    job_id TEXT,
    retry_after INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    PRIMARY KEY (pipeline_run_id, node_instance_id, request_key),
    FOREIGN KEY (pipeline_run_id, node_instance_id)
        REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

INSERT INTO workflow_automatic_agents_by_run
SELECT pipeline_run_id, node_instance_id, request_key, plan_key, payload, job_id, retry_after, reason
FROM workflow_automatic_agents;
DROP TABLE workflow_automatic_agents;
ALTER TABLE workflow_automatic_agents_by_run RENAME TO workflow_automatic_agents;
CREATE INDEX workflow_automatic_agents_ready ON workflow_automatic_agents(retry_after)
    WHERE job_id IS NULL;
