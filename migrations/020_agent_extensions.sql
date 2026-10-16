CREATE TABLE IF NOT EXISTS agent_extension_settings (
 user_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, config_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_extension_credentials (
 user_id TEXT NOT NULL, server_name TEXT NOT NULL, endpoint TEXT NOT NULL,
 bearer_token BLOB NOT NULL, PRIMARY KEY(user_id,server_name)
);
CREATE TABLE IF NOT EXISTS agent_extension_approvals (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL, call_id TEXT NOT NULL, task_json TEXT NOT NULL, tool_name TEXT NOT NULL,
 arguments_json TEXT NOT NULL, destination TEXT NOT NULL, config_revision INTEGER NOT NULL,
 policy_revision INTEGER NOT NULL, state TEXT NOT NULL, review_json TEXT,
 created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
 UNIQUE(session_id,call_id)
);
CREATE INDEX IF NOT EXISTS agent_extension_approval_session ON agent_extension_approvals(session_id,created_at);
CREATE TABLE IF NOT EXISTS agent_workflow_events (
 id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
 task_id TEXT NOT NULL, parent_call_id TEXT NOT NULL, kind TEXT NOT NULL, content TEXT NOT NULL, created_at INTEGER NOT NULL
);
PRAGMA user_version = 20;
