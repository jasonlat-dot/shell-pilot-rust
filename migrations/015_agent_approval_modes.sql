BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS agent_execution_policies_v15 (
 session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id), task_id TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('deny','confirm','auto','fullAccess')), revision INTEGER NOT NULL
);
INSERT OR REPLACE INTO agent_execution_policies_v15 SELECT session_id,task_id,mode,revision FROM agent_execution_policies;
DROP TABLE agent_execution_policies;
ALTER TABLE agent_execution_policies_v15 RENAME TO agent_execution_policies;
CREATE TABLE IF NOT EXISTS agent_execution_policy_reviews (
 request_id TEXT PRIMARY KEY REFERENCES agent_execution_requests(id),
 mode TEXT NOT NULL CHECK(mode IN ('auto','fullAccess')), revision INTEGER NOT NULL,
 decision TEXT NOT NULL, risk TEXT NOT NULL, reason TEXT NOT NULL
);
PRAGMA user_version=15;
COMMIT;
