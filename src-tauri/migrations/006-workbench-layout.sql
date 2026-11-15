CREATE TABLE workbench_layouts (
    checkout_id TEXT NOT NULL REFERENCES checkouts(id) ON DELETE CASCADE,
    workspace TEXT NOT NULL CHECK (length(workspace) BETWEEN 1 AND 64),
    layout TEXT NOT NULL CHECK (json_type(layout) = 'object'),
    PRIMARY KEY (checkout_id, workspace)
) STRICT, WITHOUT ROWID;
