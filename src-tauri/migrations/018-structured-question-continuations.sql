CREATE TABLE structured_question_continuations (
    request_event_seq INTEGER PRIMARY KEY REFERENCES agent_questions(request_event_seq),
    context_id TEXT NOT NULL REFERENCES conversation_contexts(id),
    continuation_run_id TEXT UNIQUE REFERENCES agent_attempts(run_id),
    reply_event_seq INTEGER UNIQUE REFERENCES agent_events(seq),
    CHECK ((continuation_run_id IS NULL) = (reply_event_seq IS NULL))
) STRICT;

INSERT INTO structured_question_continuations(request_event_seq, context_id)
SELECT q.request_event_seq, s.cursor_context_id
FROM agent_questions q
JOIN agent_attempts a ON a.run_id = q.run_id
JOIN agent_sessions s ON s.id = a.agent_session_id
JOIN conversations c ON c.id = a.conversation_id
JOIN runs r ON r.id = a.run_id
WHERE q.route = 'structured_turn' AND q.delivery = 'pending'
  AND r.status = 'succeeded' AND a.provider_terminal = 'completed'
  AND s.cursor_context_id = c.active_context_id
  AND NOT EXISTS (
      SELECT 1 FROM agent_attempts newer
      WHERE newer.conversation_id = a.conversation_id AND newer.ordinal > a.ordinal
  );
