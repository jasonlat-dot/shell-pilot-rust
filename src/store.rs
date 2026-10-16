use crate::{
    error::Error,
    model::{Connection, ConnectionRequest, Credentials},
    secrets,
};
use rusqlite::{params, Connection as Database, OptionalExtension};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

pub struct Store {
    pub(crate) database: Mutex<Database>,
    pub(crate) data_directory: Option<PathBuf>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut store = Self::initialize(Database::open(path)?)?;
        store.data_directory = Some(std::fs::canonicalize(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?);
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, Error> {
        Self::initialize(Database::open_in_memory()?)
    }

    fn initialize(database: Database) -> Result<Self, Error> {
        database.busy_timeout(Duration::from_secs(5))?;
        database.execute_batch(include_str!("../migrations/001_connections.sql"))?;
        database.execute_batch(include_str!("../migrations/002_files.sql"))?;
        database.execute_batch(include_str!("../migrations/003_agent_context.sql"))?;
        database.execute_batch(include_str!("../migrations/004_agent_outputs.sql"))?;
        database.execute_batch(include_str!("../migrations/005_agent_models.sql"))?;
        database.execute_batch(include_str!("../migrations/006_agent_turns.sql"))?;
        database.execute_batch(include_str!("../migrations/007_agent_model_names.sql"))?;
        database.execute_batch(include_str!("../migrations/008_agent_responses.sql"))?;
        database.execute_batch(include_str!("../migrations/009_agent_scopes.sql"))?;
        database.execute_batch(include_str!("../migrations/010_agent_projects.sql"))?;
        database.execute_batch(include_str!("../migrations/011_agent_context_status.sql"))?;
        database.execute_batch(include_str!(
            "../migrations/012_agent_execution_approvals.sql"
        ))?;
        database.execute_batch(include_str!("../migrations/013_agent_tool_turns.sql"))?;
        database.execute_batch(include_str!("../migrations/014_agent_steering.sql"))?;
        database.execute_batch(include_str!("../migrations/015_agent_approval_modes.sql"))?;
        database.execute_batch(include_str!("../migrations/016_agent_removed_projects.sql"))?;
        database.execute_batch(include_str!("../migrations/017_agent_attachments.sql"))?;
        database.execute_batch(include_str!("../migrations/018_agent_memories.sql"))?;
        database.execute_batch(include_str!("../migrations/019_agent_chat_titles.sql"))?;
        database.execute_batch(include_str!("../migrations/020_agent_extensions.sql"))?;
        Ok(Self {
            database: Mutex::new(database),
            data_directory: None,
        })
    }

    fn read(database: &Database, id: &str) -> Result<Connection, Error> {
        let json: Option<String> = database
            .query_row(
                "SELECT metadata FROM ssh_connections WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(&json.ok_or(Error::NotFound)?)?)
    }

    pub fn get(&self, id: &str) -> Result<Connection, Error> {
        let database = self.database.lock().map_err(|_| Error::Worker)?;
        Self::read(&database, id)
    }

    pub fn list(&self, user_id: &str) -> Result<Vec<Connection>, Error> {
        let database = self.database.lock().map_err(|_| Error::Worker)?;
        let mut statement = database
            .prepare("SELECT metadata FROM ssh_connections WHERE user_id=?1 ORDER BY rowid")?;
        let rows = statement.query_map([user_id], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn save(&self, request: ConnectionRequest, update: bool) -> Result<Connection, Error> {
        let id = if update {
            request
                .connection_id
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .ok_or_else(|| Error::Invalid("更新时必须提供 connectionId".into()))?
        } else {
            request
                .connection_id
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
        };
        let mut database = self.database.lock().map_err(|_| Error::Worker)?;
        let transaction = database.transaction()?;
        let previous = if update {
            Some(Self::read(&transaction, &id)?)
        } else {
            if transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM ssh_connections WHERE id=?1)",
                [&id],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error::Conflict);
            }
            None
        };
        let connection = request.resolve(id.clone(), previous.as_ref())?;
        let prior_secrets: (Option<Vec<u8>>, Option<Vec<u8>>) = if update {
            transaction.query_row(
                "SELECT password, private_key FROM ssh_connections WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
        } else {
            (None, None)
        };
        let encrypt = |value: Option<String>,
                       prior: Option<Vec<u8>>,
                       kind: &str|
         -> Result<Option<Vec<u8>>, Error> {
            match value.filter(|s| !s.is_empty()) {
                Some(s) => Ok(Some(secrets::protect(&s, &format!("{id}:{kind}"))?)),
                None => Ok(prior),
            }
        };
        let password = encrypt(request.password, prior_secrets.0, "password")?;
        let private_key = encrypt(request.private_key, prior_secrets.1, "private_key")?;
        transaction.execute("INSERT INTO ssh_connections(id,user_id,metadata,password,private_key) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET user_id=excluded.user_id, metadata=excluded.metadata, password=excluded.password, private_key=excluded.private_key",
            params![id, connection.user_id, serde_json::to_string(&connection)?, password, private_key])?;
        transaction.commit()?;
        Ok(connection)
    }

    pub fn delete(&self, id: &str) -> Result<(), Error> {
        let database = self.database.lock().map_err(|_| Error::Worker)?;
        if database.execute("DELETE FROM ssh_connections WHERE id=?1", [id])? == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    pub fn credentials(&self, id: &str) -> Result<Credentials, Error> {
        let database = self.database.lock().map_err(|_| Error::Worker)?;
        let (password, private_key): (Option<Vec<u8>>, Option<Vec<u8>>) = database
            .query_row(
                "SELECT password, private_key FROM ssh_connections WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let decrypt = |value: Option<Vec<u8>>, kind: &str| -> Result<Option<String>, Error> {
            Ok(value
                .map(|v| secrets::unprotect(&v, &format!("{id}:{kind}")))
                .transpose()?)
        };
        Ok(Credentials {
            password: decrypt(password, "password")?,
            private_key: decrypt(private_key, "private_key")?,
        })
    }
}
