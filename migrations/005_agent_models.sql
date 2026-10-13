CREATE TABLE IF NOT EXISTS agent_model_profiles (
 id TEXT NOT NULL, user_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision > 0),
 config_json TEXT NOT NULL, api_key BLOB, updated_at INTEGER NOT NULL,
 PRIMARY KEY(user_id,id)
);
PRAGMA user_version = 5;
