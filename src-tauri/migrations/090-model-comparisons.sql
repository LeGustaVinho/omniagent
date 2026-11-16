CREATE TABLE model_comparison_groups (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    source_checkout_id TEXT NOT NULL,
    commit_oid TEXT NOT NULL,
    tree_oid TEXT NOT NULL,
    comparison_checkout TEXT NOT NULL UNIQUE,
    prompt BLOB NOT NULL,
    FOREIGN KEY (project_id, source_checkout_id) REFERENCES checkouts(project_id, id)
) STRICT;

CREATE INDEX model_comparison_groups_by_project ON model_comparison_groups(project_id);

CREATE TABLE model_comparison_candidates (
    group_id TEXT NOT NULL REFERENCES model_comparison_groups(id),
    position INTEGER NOT NULL CHECK (position >= 0),
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    selection TEXT NOT NULL CHECK (json_type(selection) = 'object'),
    PRIMARY KEY (group_id, position)
) STRICT;
