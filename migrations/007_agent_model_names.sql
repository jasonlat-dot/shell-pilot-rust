-- Editable labels remain separate from stable IDs used by credentials and sessions.
CREATE TABLE IF NOT EXISTS agent_model_names (
 user_id TEXT NOT NULL,
 profile_id TEXT NOT NULL,
 display_name TEXT NOT NULL,
 PRIMARY KEY(user_id,profile_id),
 FOREIGN KEY(user_id,profile_id) REFERENCES agent_model_profiles(user_id,id) ON DELETE CASCADE
);
PRAGMA user_version = 7;
