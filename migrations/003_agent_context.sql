CREATE TABLE IF NOT EXISTS agent_sessions (
 id TEXT PRIMARY KEY, user_id TEXT NOT NULL, agent_id TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0),
 task_json TEXT, checkpoint_id TEXT, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_events (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 session_id TEXT NOT NULL REFERENCES agent_sessions(id),
 task_id TEXT, request_id TEXT NOT NULL, kind TEXT NOT NULL, call_id TEXT,
 payload TEXT NOT NULL, created_at INTEGER NOT NULL,
 UNIQUE(session_id, request_id)
);
CREATE INDEX IF NOT EXISTS agent_events_session ON agent_events(session_id,id);
CREATE UNIQUE INDEX IF NOT EXISTS agent_call_once
 ON agent_events(session_id,call_id) WHERE kind='tool_call';
CREATE UNIQUE INDEX IF NOT EXISTS agent_result_once
 ON agent_events(session_id,call_id) WHERE kind='tool_result';
CREATE TABLE IF NOT EXISTS agent_checkpoints (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES agent_sessions(id),
 through_event_id INTEGER NOT NULL, source_revision INTEGER NOT NULL,
 committed_revision INTEGER NOT NULL, content TEXT NOT NULL, created_at INTEGER NOT NULL,
 UNIQUE(session_id,source_revision)
);
PRAGMA user_version = 3;
