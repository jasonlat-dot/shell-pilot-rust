CREATE TABLE IF NOT EXISTS agent_model_transitions (
    session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id),
    through_event_id INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_turn_errors (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    message TEXT NOT NULL,
    PRIMARY KEY(session_id, request_id),
    FOREIGN KEY(session_id, request_id) REFERENCES agent_chat_turns(session_id, request_id)
);
PRAGMA user_version = 23;
