CREATE TABLE IF NOT EXISTS agent_context_status (
 session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id),
 owner TEXT NOT NULL, status_json TEXT NOT NULL, lease_until INTEGER NOT NULL
);
PRAGMA user_version=11;
