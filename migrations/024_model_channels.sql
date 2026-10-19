CREATE TABLE IF NOT EXISTS agent_model_channels (
  user_id TEXT NOT NULL,
  id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision > 0),
  PRIMARY KEY(user_id,id)
);
CREATE TABLE IF NOT EXISTS agent_model_channel_members (
  user_id TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  model TEXT NOT NULL,
  position INTEGER NOT NULL,
  active INTEGER NOT NULL CHECK(active IN (0,1)),
  PRIMARY KEY(user_id,profile_id),
  UNIQUE(user_id,channel_id,model),
  FOREIGN KEY(user_id,channel_id) REFERENCES agent_model_channels(user_id,id) ON DELETE CASCADE,
  FOREIGN KEY(user_id,profile_id) REFERENCES agent_model_profiles(user_id,id) ON DELETE CASCADE
);
PRAGMA user_version = 24;
