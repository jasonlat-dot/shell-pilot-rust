PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS ssh_connections (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    metadata TEXT NOT NULL,
    password BLOB,
    private_key BLOB
);
CREATE INDEX IF NOT EXISTS idx_ssh_connections_user ON ssh_connections(user_id);
PRAGMA user_version = 1;
