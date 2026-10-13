CREATE TABLE IF NOT EXISTS agent_outputs (
 id TEXT PRIMARY KEY,
 session_id TEXT NOT NULL REFERENCES agent_sessions(id),
 task_id TEXT NOT NULL, call_id TEXT NOT NULL,
 channel TEXT NOT NULL CHECK(channel IN ('stdout','stderr')),
 request_id TEXT NOT NULL, writer_token TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('writing','complete')),
 size INTEGER NOT NULL DEFAULT 0 CHECK(size >= 0),
 max_bytes INTEGER NOT NULL CHECK(max_bytes > 0),
 expires_at INTEGER NOT NULL,
 sha256 TEXT, preview TEXT, preview_truncated INTEGER, preview_lossy INTEGER,
 event_id INTEGER REFERENCES agent_events(id), created_at INTEGER NOT NULL,
 UNIQUE(session_id,request_id), UNIQUE(session_id,call_id,channel)
);
CREATE INDEX IF NOT EXISTS agent_outputs_owner ON agent_outputs(session_id,id);
CREATE INDEX IF NOT EXISTS agent_outputs_expiry ON agent_outputs(state,expires_at);
CREATE TABLE IF NOT EXISTS agent_output_chunks (
 output_id TEXT NOT NULL REFERENCES agent_outputs(id),
 chunk_index INTEGER NOT NULL CHECK(chunk_index >= 0),
 data BLOB NOT NULL CHECK(length(data) > 0 AND length(data) <= 65536),
 sha256 TEXT NOT NULL CHECK(length(sha256)=64),
 PRIMARY KEY(output_id,chunk_index)
);
PRAGMA user_version = 4;
