CREATE TABLE model_comparison_syntheses (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES model_comparison_groups(id),
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    selection TEXT NOT NULL CHECK (json_type(selection) = 'object'),
    prompt BLOB NOT NULL
) STRICT;

CREATE INDEX model_comparison_syntheses_by_group ON model_comparison_syntheses(group_id);

CREATE TABLE model_comparison_inputs (
    job_id TEXT NOT NULL REFERENCES jobs(id),
    position INTEGER NOT NULL CHECK (position >= 0),
    group_id TEXT NOT NULL REFERENCES model_comparison_groups(id),
    source_job_id TEXT NOT NULL REFERENCES jobs(id),
    source_run_id TEXT NOT NULL REFERENCES runs(id),
    source_dispatch_id TEXT NOT NULL,
    response_revision_id TEXT NOT NULL,
    PRIMARY KEY (job_id, position),
    UNIQUE (job_id, source_job_id)
) STRICT;

CREATE INDEX model_comparison_inputs_by_source ON model_comparison_inputs(source_run_id);
