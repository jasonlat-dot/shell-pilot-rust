CREATE TABLE IF NOT EXISTS agent_model_channel_states (
  user_id TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  PRIMARY KEY(user_id,channel_id),
  FOREIGN KEY(user_id,channel_id) REFERENCES agent_model_profiles(user_id,id) ON DELETE CASCADE
);
PRAGMA user_version = 26;
