CREATE TABLE IF NOT EXISTS agent_chat_turns (
 session_id TEXT NOT NULL REFERENCES agent_sessions(id),
 request_id TEXT NOT NULL,
 input_json TEXT NOT NULL,
 owner TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('running','completed','cancelled','failed','interrupted')),
 cancel_requested INTEGER NOT NULL DEFAULT 0,
 partial_text TEXT NOT NULL DEFAULT '',
 result_text TEXT,
 error_code TEXT,
 usage_json TEXT,
 user_event_id INTEGER NOT NULL,
 assistant_event_id INTEGER,
 revision INTEGER NOT NULL,
 task_id TEXT,
 lease_until INTEGER NOT NULL,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY(session_id, request_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS agent_one_running_turn ON agent_chat_turns(session_id) WHERE state='running';
CREATE INDEX IF NOT EXISTS agent_turn_history ON agent_chat_turns(session_id,user_event_id);
PRAGMA user_version = 6;
