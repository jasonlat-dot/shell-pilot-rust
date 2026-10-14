CREATE TABLE IF NOT EXISTS agent_projects (
 id TEXT PRIMARY KEY, user_id TEXT NOT NULL, name TEXT NOT NULL,
 connection_id TEXT NOT NULL, identity_json TEXT, profile_json TEXT,
 imported INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_projects_owner ON agent_projects(user_id);
CREATE TABLE IF NOT EXISTS agent_project_chats (
 session_id TEXT PRIMARY KEY REFERENCES agent_sessions(id),
 project_id TEXT NOT NULL REFERENCES agent_projects(id)
);
CREATE INDEX IF NOT EXISTS agent_project_history ON agent_project_chats(project_id);
-- Preserve existing IDs/events/checkpoints; only add sidebar membership. Distinct verified
-- identities of one saved connection become distinct projects.
INSERT OR IGNORE INTO agent_projects(id,user_id,name,connection_id,identity_json,imported,created_at)
 SELECT 'scope-' || MIN(a.id),a.user_id,
 COALESCE((SELECT json_extract(metadata,'$.connectionName') FROM ssh_connections WHERE id=s.connection_id),'服务器历史'),
 s.connection_id,s.identity_json,1,MIN(a.created_at)
 FROM agent_sessions a JOIN agent_session_scopes s ON s.session_id=a.id
 WHERE s.kind='server' AND NOT EXISTS(SELECT 1 FROM agent_project_chats c WHERE c.session_id=a.id)
 GROUP BY a.user_id,s.connection_id,s.identity_json;
INSERT OR IGNORE INTO agent_project_chats(session_id,project_id)
 SELECT a.id,p.id FROM agent_sessions a JOIN agent_session_scopes s ON s.session_id=a.id
 JOIN agent_projects p ON p.user_id=a.user_id AND p.connection_id=s.connection_id
 AND p.identity_json=s.identity_json AND p.imported=1 WHERE s.kind='server';
-- Older unverified task histories are archived without inventing a verified identity.
INSERT OR IGNORE INTO agent_projects(id,user_id,name,connection_id,imported,created_at)
 SELECT 'legacy-' || MIN(a.id),a.user_id,'旧服务器历史',json_extract(a.task_json,'$.target.connectionId'),1,MIN(a.created_at)
 FROM agent_sessions a WHERE json_extract(a.task_json,'$.target.connectionId') IS NOT NULL
 AND NOT EXISTS(SELECT 1 FROM agent_session_scopes s WHERE s.session_id=a.id)
 AND NOT EXISTS(SELECT 1 FROM agent_project_chats c WHERE c.session_id=a.id)
 GROUP BY a.user_id,json_extract(a.task_json,'$.target.connectionId');
INSERT OR IGNORE INTO agent_project_chats(session_id,project_id)
 SELECT a.id,p.id FROM agent_sessions a JOIN agent_projects p
 ON p.user_id=a.user_id AND p.connection_id=json_extract(a.task_json,'$.target.connectionId')
 AND p.identity_json IS NULL AND p.imported=1
 WHERE NOT EXISTS(SELECT 1 FROM agent_session_scopes s WHERE s.session_id=a.id);
PRAGMA user_version=10;
