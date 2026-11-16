CREATE TABLE workflow_mcp_servers (
    project_id TEXT NOT NULL REFERENCES projects(id),
    id TEXT NOT NULL,
    revision TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (project_id, id, revision)
) STRICT;

CREATE TABLE workflow_mcp_grants (
    project_id TEXT NOT NULL,
    server_id TEXT NOT NULL,
    server_revision TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    contract_revision TEXT NOT NULL,
    destination_digest TEXT NOT NULL,
    id TEXT NOT NULL UNIQUE,
    PRIMARY KEY (project_id, server_id, server_revision, tool_name, contract_revision, destination_digest),
    FOREIGN KEY (project_id, server_id, server_revision) REFERENCES workflow_mcp_servers(project_id, id, revision)
) STRICT;

CREATE TABLE workflow_mcp_jobs (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    pipeline_run_id TEXT NOT NULL,
    node_instance_id TEXT NOT NULL,
    execution_digest TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('pending','approved','denied','preauthorized')),
    grant_id TEXT,
    payload BLOB NOT NULL,
    FOREIGN KEY (pipeline_run_id, node_instance_id) REFERENCES workflow_node_pins(pipeline_run_id, node_instance_id),
    UNIQUE (pipeline_run_id, node_instance_id, execution_digest)
) STRICT;

CREATE TABLE workflow_mcp_attempts (
    run_id TEXT PRIMARY KEY REFERENCES runs(id),
    job_id TEXT NOT NULL REFERENCES workflow_mcp_jobs(job_id),
    state TEXT NOT NULL CHECK (state IN ('preparing','not_sent','sent','confirmed','rejected','uncertain','reconciled')),
    artifact_id TEXT UNIQUE,
    payload BLOB
) STRICT;
