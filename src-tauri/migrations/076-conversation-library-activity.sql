CREATE TABLE conversation_library_activity (
    entry_order INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL UNIQUE REFERENCES conversations(id) ON DELETE CASCADE,
    last_activity_at INTEGER NOT NULL
) STRICT;

INSERT INTO conversation_library_activity(conversation_id, last_activity_at)
SELECT conversation.id, 1000 * MAX(MAX(run.created_at, COALESCE(event.recorded_at, run.created_at)))
FROM conversations conversation
JOIN agent_attempts attempt ON attempt.conversation_id = conversation.id
JOIN runs run ON run.id = attempt.run_id
LEFT JOIN turns turn ON turn.run_id = run.id
LEFT JOIN run_events event ON event.turn_id = turn.id
WHERE conversation.background = 0
GROUP BY conversation.id
ORDER BY conversation.id;

CREATE TRIGGER conversation_library_create AFTER INSERT ON conversations
WHEN new.background = 0 BEGIN
    INSERT INTO conversation_library_activity(conversation_id, last_activity_at)
    VALUES (new.id, CAST(unixepoch('subsec') * 1000 AS INTEGER));
END;

CREATE TRIGGER conversation_library_edit AFTER UPDATE OF title, active_context_id ON conversations
WHEN new.background = 0 AND (old.title IS NOT new.title OR old.active_context_id IS NOT new.active_context_id) BEGIN
    INSERT INTO conversation_library_activity(conversation_id, last_activity_at)
    VALUES (new.id, CAST(unixepoch('subsec') * 1000 AS INTEGER))
    ON CONFLICT (conversation_id) DO UPDATE
      SET entry_order = excluded.entry_order, last_activity_at = MAX(last_activity_at, excluded.last_activity_at);
END;

CREATE TRIGGER conversation_library_attempt AFTER INSERT ON agent_attempts
WHEN EXISTS (SELECT 1 FROM conversations WHERE id = new.conversation_id AND background = 0) BEGIN
    INSERT INTO conversation_library_activity(conversation_id, last_activity_at)
    VALUES (new.conversation_id, CAST(unixepoch('subsec') * 1000 AS INTEGER))
    ON CONFLICT (conversation_id) DO UPDATE
      SET entry_order = excluded.entry_order, last_activity_at = MAX(last_activity_at, excluded.last_activity_at);
END;

CREATE TRIGGER conversation_library_run_state AFTER UPDATE OF status ON runs
WHEN old.status IS NOT new.status BEGIN
    INSERT INTO conversation_library_activity(conversation_id, last_activity_at)
    SELECT attempt.conversation_id, CAST(unixepoch('subsec') * 1000 AS INTEGER)
    FROM agent_attempts attempt JOIN conversations conversation ON conversation.id = attempt.conversation_id
    WHERE attempt.run_id = new.id AND conversation.background = 0
    ON CONFLICT (conversation_id) DO UPDATE
      SET entry_order = excluded.entry_order, last_activity_at = MAX(last_activity_at, excluded.last_activity_at);
END;
