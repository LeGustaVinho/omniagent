DROP VIEW unconfirmed_agent_stops;

CREATE VIEW unconfirmed_agent_stops AS
SELECT run_id, conversation_id FROM unconfirmed_question_stops
UNION
SELECT held.run_id, a.conversation_id FROM unconfirmed_managed_goal_stops held
JOIN agent_attempts a ON a.run_id = held.run_id
UNION
SELECT x.run_id, a.conversation_id FROM native_goal_executors x
JOIN agent_attempts a ON a.run_id = x.run_id
WHERE NOT x.stop_confirmed;
