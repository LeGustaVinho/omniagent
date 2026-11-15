ALTER TABLE instruction_delivery_attempts RENAME TO previous_instruction_delivery_attempts;

CREATE TABLE instruction_delivery_attempts (
    job_id TEXT NOT NULL REFERENCES queued_conversation_instructions(job_id),
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    revision_id TEXT NOT NULL REFERENCES message_revisions(id),
    policy_key TEXT NOT NULL,
    route TEXT NOT NULL CHECK (route IN ('standalone', 'continuation', 'steering')),
    superseded INTEGER NOT NULL DEFAULT 0 CHECK (superseded IN (0, 1)),
    provider_session_id TEXT,
    provider_turn_id TEXT,
    steering_state TEXT CHECK (steering_state IN ('claimed', 'prepared', 'accepted', 'rejected', 'unconfirmed')),
    prepared_event_seq INTEGER REFERENCES agent_events(seq),
    settled_event_seq INTEGER REFERENCES agent_events(seq),
    review_resolution TEXT CHECK (review_resolution IN ('undelivered', 'discarded')),
    reviewed_ms INTEGER,
    CHECK (
        (route = 'steering' AND provider_session_id IS NOT NULL AND provider_turn_id IS NOT NULL AND steering_state IS NOT NULL)
        OR (route <> 'steering' AND provider_session_id IS NULL AND provider_turn_id IS NULL
            AND steering_state IS NULL AND prepared_event_seq IS NULL AND settled_event_seq IS NULL)
    ),
    CHECK (steering_state NOT IN ('prepared', 'accepted', 'unconfirmed') OR prepared_event_seq IS NOT NULL),
    CHECK (steering_state <> 'accepted' OR settled_event_seq IS NOT NULL),
    CHECK ((reviewed_ms IS NULL) = (review_resolution IS NULL)),
    CHECK (review_resolution IS NULL OR (steering_state = 'unconfirmed' AND superseded = 1)),
    PRIMARY KEY (job_id, run_id)
) STRICT, WITHOUT ROWID;

INSERT INTO instruction_delivery_attempts (job_id, run_id, revision_id, policy_key, route, superseded)
SELECT job_id, run_id, revision_id, policy_key, route, superseded FROM previous_instruction_delivery_attempts;

DROP TABLE previous_instruction_delivery_attempts;

CREATE UNIQUE INDEX instruction_current_delivery
ON instruction_delivery_attempts(job_id) WHERE superseded = 0;

CREATE INDEX instruction_deliveries_by_run ON instruction_delivery_attempts(run_id, job_id);

CREATE UNIQUE INDEX one_pending_steer_per_run ON instruction_delivery_attempts(run_id)
WHERE steering_state IN ('claimed', 'prepared');

CREATE VIEW borrowed_instruction_deliveries AS
SELECT i.job_id, i.run_id, i.revision_id, i.route,
       CASE WHEN i.route = 'steering' THEN i.steering_state = 'accepted'
            ELSE EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                         WHERE s.run_id = i.run_id AND d.ordinal = 1 AND d.delivery = 'submitted')
       END AS delivered,
       CASE WHEN i.route = 'steering' THEN i.steering_state = 'unconfirmed'
            ELSE EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                         WHERE s.run_id = i.run_id AND d.ordinal = 1 AND d.delivery = 'unconfirmed')
       END AS uncertain
FROM instruction_delivery_attempts i
WHERE i.route IN ('continuation', 'steering') AND i.superseded = 0;
