-- Accepted input belongs to an exact running turn, not to the currently selected tab.
CREATE TABLE IF NOT EXISTS agent_turn_inputs (
 session_id TEXT NOT NULL, request_id TEXT NOT NULL, id TEXT NOT NULL,
 message TEXT NOT NULL, event_id INTEGER, created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id,id),
 FOREIGN KEY(session_id,request_id) REFERENCES agent_chat_turns(session_id,request_id)
);
CREATE TABLE IF NOT EXISTS agent_input_gates (
 session_id TEXT NOT NULL, request_id TEXT NOT NULL,
 PRIMARY KEY(session_id,request_id),
 FOREIGN KEY(session_id,request_id) REFERENCES agent_chat_turns(session_id,request_id)
);
PRAGMA user_version=14;
