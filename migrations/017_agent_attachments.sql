-- Immutable model-ready snapshots; original uploads remain in the file service.
CREATE TABLE IF NOT EXISTS agent_attachments(
 session_id TEXT NOT NULL REFERENCES agent_sessions(id),file_id TEXT NOT NULL,user_id TEXT NOT NULL,
 reference_json TEXT NOT NULL,content_json TEXT NOT NULL,created_at INTEGER NOT NULL,
 PRIMARY KEY(session_id,file_id));
CREATE TABLE IF NOT EXISTS agent_event_attachments(
 event_id INTEGER NOT NULL REFERENCES agent_events(id),session_id TEXT NOT NULL,file_id TEXT NOT NULL,
 position INTEGER NOT NULL,PRIMARY KEY(event_id,file_id),
 FOREIGN KEY(session_id,file_id) REFERENCES agent_attachments(session_id,file_id));
PRAGMA user_version=17;
