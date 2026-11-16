CREATE TABLE generated_image_evidence_payloads (
    event_seq INTEGER PRIMARY KEY REFERENCES agent_events(seq) ON DELETE CASCADE,
    payload BLOB NOT NULL
) STRICT;

CREATE TABLE side_output_fixture_payloads (
    run_id TEXT PRIMARY KEY REFERENCES job_side_responses(run_id) ON DELETE CASCADE,
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;
