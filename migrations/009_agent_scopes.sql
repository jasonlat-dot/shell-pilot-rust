-- Each scope owns its own existing event log/checkpoints/provider windows.
CREATE TABLE IF NOT EXISTS agent_session_scopes (
 session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id),
 root_id TEXT NOT NULL REFERENCES agent_sessions(id),
 kind TEXT NOT NULL CHECK(kind IN ('general','server')),
 connection_id TEXT,
 identity_json TEXT,
 origin_terminal_id TEXT
);
CREATE INDEX IF NOT EXISTS agent_scope_roots ON agent_session_scopes(root_id);
PRAGMA user_version=9;
