-- Provider-owned replay state never enters visible history, IPC or diagnostics.
CREATE TABLE IF NOT EXISTS agent_provider_continuations (
 session_id TEXT NOT NULL REFERENCES agent_sessions(id),
 after_event_id INTEGER NOT NULL,
 through_event_id INTEGER NOT NULL REFERENCES agent_events(id),
 state TEXT NOT NULL,
 PRIMARY KEY(session_id, through_event_id)
);
PRAGMA user_version = 22;
