CREATE TABLE job_side_requests (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    conversation_id TEXT NOT NULL,
    context_id TEXT NOT NULL,
    source_run_id TEXT NOT NULL REFERENCES agent_attempts(run_id),
    purpose TEXT NOT NULL CHECK (purpose IN ('question', 'suggestion')),
    payload BLOB NOT NULL,
    discarded INTEGER NOT NULL DEFAULT 0 CHECK (discarded IN (0, 1)),
    FOREIGN KEY (conversation_id, context_id) REFERENCES conversation_contexts(conversation_id, id)
) STRICT, WITHOUT ROWID;

CREATE INDEX side_requests_by_conversation ON job_side_requests(conversation_id, job_id);
CREATE UNIQUE INDEX one_suggestion_per_source ON job_side_requests(conversation_id, context_id)
WHERE purpose = 'suggestion';

CREATE TABLE job_side_responses (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id),
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE composer_suggestions_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    mode TEXT NOT NULL CHECK (mode IN ('off', 'native', 'omniagent'))
) STRICT;
INSERT INTO composer_suggestions_settings VALUES (1, 'native');

CREATE TRIGGER discard_suggestion_on_draft_insert AFTER INSERT ON drafts
WHEN NEW.kind = 'conversation'
BEGIN
    UPDATE job_side_requests SET discarded = 1
    WHERE conversation_id = NEW.owner_id AND purpose = 'suggestion' AND NOT discarded;
END;

CREATE TRIGGER discard_suggestion_on_draft_edit AFTER UPDATE ON drafts
WHEN NEW.kind = 'conversation'
BEGIN
    UPDATE job_side_requests SET discarded = 1
    WHERE conversation_id = NEW.owner_id AND purpose = 'suggestion' AND NOT discarded;
END;

CREATE TRIGGER discard_suggestion_on_context_change AFTER UPDATE OF active_context_id ON conversations
WHEN NEW.active_context_id IS NOT OLD.active_context_id
BEGIN
    UPDATE job_side_requests SET discarded = 1
    WHERE conversation_id = NEW.id AND purpose = 'suggestion' AND NOT discarded
      AND context_id IS NOT NEW.active_context_id;
END;

CREATE TRIGGER retire_discarded_suggestion AFTER UPDATE OF discarded ON job_side_requests
WHEN NEW.discarded AND NOT OLD.discarded
BEGIN
    UPDATE jobs SET queue_state = 'cancelled' WHERE id = NEW.job_id
        AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = NEW.job_id);
    UPDATE job_attempts SET cancel_requested = 1 WHERE job_id = NEW.job_id
        AND EXISTS (SELECT 1 FROM runs WHERE id = job_attempts.run_id AND status IN ('starting', 'running'));
END;
