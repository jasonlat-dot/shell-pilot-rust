-- Marks tool turns so desktop lifecycle can finalize their independently journaled events.
CREATE TABLE IF NOT EXISTS agent_tool_turns (
 session_id TEXT NOT NULL, request_id TEXT NOT NULL,
 PRIMARY KEY(session_id,request_id),
 FOREIGN KEY(session_id,request_id) REFERENCES agent_chat_turns(session_id,request_id)
);
PRAGMA user_version=13;
