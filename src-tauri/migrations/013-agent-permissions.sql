CREATE TABLE agent_permissions (
    request_event_seq INTEGER PRIMARY KEY REFERENCES agent_events(seq),
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    turn_id TEXT NOT NULL REFERENCES turns(id),
    request_key BLOB NOT NULL,
    decision TEXT CHECK (decision IN ('allow_once', 'deny', 'cancel')),
    delivery TEXT NOT NULL CHECK (delivery IN ('pending', 'prepared', 'submitted', 'unconfirmed', 'closed')),
    CHECK ((delivery IN ('pending', 'closed')) = (decision IS NULL))
) STRICT;

CREATE UNIQUE INDEX agent_permissions_pending ON agent_permissions(run_id, request_key)
    WHERE delivery IN ('pending', 'prepared');
