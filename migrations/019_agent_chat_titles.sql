-- User titles are sidebar metadata, independent of conversation/model input.
CREATE TABLE IF NOT EXISTS agent_chat_titles (
 session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id) ON DELETE CASCADE,
 title TEXT NOT NULL, updated_at INTEGER NOT NULL
);
PRAGMA user_version=19;
