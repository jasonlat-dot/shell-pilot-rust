-- Native-only complete Responses output windows; not part of IPC or visible text events.
CREATE TABLE IF NOT EXISTS agent_responses_checkpoints (
 checkpoint_id TEXT PRIMARY KEY REFERENCES agent_checkpoints(id),
 binding TEXT NOT NULL, output TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_responses_outputs (
 event_id INTEGER PRIMARY KEY REFERENCES agent_events(id),
 binding TEXT NOT NULL, output TEXT NOT NULL
);
PRAGMA user_version = 8;