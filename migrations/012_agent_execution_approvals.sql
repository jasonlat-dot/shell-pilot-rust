PRAGMA synchronous = FULL;
-- Durable native-only decisions. Approval never comes from model arguments/history.
CREATE TABLE IF NOT EXISTS agent_execution_requests (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES agent_sessions(id),
 user_id TEXT NOT NULL, task_id TEXT NOT NULL, call_id TEXT NOT NULL,
 request_json TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN
 ('pending','approved','denied','cancelled','expired','admitted','completed','notSent','unknown')),
 grant_id TEXT, result_json TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL, UNIQUE(session_id,call_id)
);
CREATE INDEX IF NOT EXISTS agent_execution_owner ON agent_execution_requests(user_id,session_id,created_at);
CREATE TABLE IF NOT EXISTS agent_execution_policies (
 session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id), task_id TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('deny','confirm')), revision INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_execution_grants (
 request_id TEXT PRIMARY KEY REFERENCES agent_execution_requests(id),
 session_id TEXT NOT NULL REFERENCES agent_sessions(id), task_id TEXT NOT NULL,
 context_json TEXT NOT NULL, command_json TEXT NOT NULL, expires_at INTEGER NOT NULL,
 revoked INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS agent_execution_grant_scope ON agent_execution_grants(session_id,task_id);
PRAGMA user_version=12;
