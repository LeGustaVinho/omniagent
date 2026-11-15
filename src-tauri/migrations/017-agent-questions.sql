CREATE TABLE agent_questions (
    request_event_seq INTEGER PRIMARY KEY REFERENCES agent_events(seq),
    run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    turn_id TEXT NOT NULL REFERENCES turns(id),
    request_key BLOB NOT NULL,
    route TEXT NOT NULL CHECK (route IN ('native', 'structured_turn')),
    reply_event_seq INTEGER REFERENCES agent_events(seq),
    delivery TEXT NOT NULL CHECK (delivery IN ('pending', 'prepared', 'submitted', 'unconfirmed', 'closed')),
    CHECK ((delivery IN ('pending', 'closed')) = (reply_event_seq IS NULL))
) STRICT;

CREATE UNIQUE INDEX agent_questions_pending ON agent_questions(run_id, request_key)
    WHERE delivery IN ('pending', 'prepared');
