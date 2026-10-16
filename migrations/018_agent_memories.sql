-- User-confirmed durable notes are independent of task events and checkpoints.
CREATE TABLE IF NOT EXISTS agent_memories (
 id TEXT PRIMARY KEY,user_id TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),
 record_json TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS agent_memories_owner ON agent_memories(user_id,updated_at DESC);
PRAGMA user_version=18;
