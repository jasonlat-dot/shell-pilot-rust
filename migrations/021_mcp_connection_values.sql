CREATE TABLE IF NOT EXISTS agent_mcp_connection_values (
 user_id TEXT NOT NULL, server_name TEXT NOT NULL, target TEXT NOT NULL,
 encrypted_values BLOB NOT NULL, PRIMARY KEY(user_id,server_name)
);
PRAGMA user_version = 21;
