-- Keep membership so startup imports cannot recreate removed sidebar projects.
CREATE TABLE IF NOT EXISTS agent_removed_projects (
 project_id TEXT PRIMARY KEY REFERENCES agent_projects(id),
 removed_at INTEGER NOT NULL
);
PRAGMA user_version=16;
