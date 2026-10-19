//! Local request diagnostics and explicit user/assistant conversation records.
use chrono::{Local, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    future::Future,
    io::{self, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Level {
    Debug = 0,
    #[default]
    Info = 1,
    Warn = 2,
    Error = 3,
}
impl Level {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
    pub fn text(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

pub struct Diagnostics {
    directory: Mutex<Option<PathBuf>>,
    level: AtomicU8,
    sequence: AtomicU64,
    write_failed: AtomicBool,
    retention_days: AtomicU32,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            directory: Mutex::new(None),
            level: AtomicU8::new(Level::Info as u8),
            sequence: AtomicU64::new(0),
            write_failed: AtomicBool::new(false),
            retention_days: AtomicU32::new(7),
        }
    }
}
impl Diagnostics {
    pub fn configure(&self, directory: PathBuf, level: Level) -> io::Result<()> {
        fs::create_dir_all(&directory)?;
        *self
            .directory
            .lock()
            .map_err(|_| io::Error::other("diagnostic writer unavailable"))? = Some(directory);
        self.set_level(level);
        Ok(())
    }
    pub fn set_level(&self, level: Level) {
        self.level.store(level as u8, Ordering::Relaxed);
    }
    pub fn set_retention_days(&self, days: u32) -> io::Result<()> {
        if !(1..=3650).contains(&days) {
            return Err(io::Error::other("日志保留天数必须为 1 到 3650"));
        }
        self.retention_days.store(days, Ordering::Relaxed);
        Ok(())
    }
    /// Only trace files in expired ISO-date directories are eligible. Never follow
    /// symlinks/junctions or recursively remove unrelated content.
    pub fn cleanup_expired(&self) -> io::Result<usize> {
        self.cleanup_before(Local::now().date_naive())
    }
    fn cleanup_before(&self, today: chrono::NaiveDate) -> io::Result<usize> {
        let directory = self.directory.lock().map_err(|_| io::Error::other("diagnostic writer unavailable"))?;
        let Some(directory) = directory.as_ref() else { return Ok(0); };
        let root = fs::canonicalize(directory)?;
        let mut removed = 0;
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue; };
            let Ok(date) = chrono::NaiveDate::parse_from_str(name, "%Y-%m-%d") else { continue; };
            if date.format("%Y-%m-%d").to_string() != name
                || today.signed_duration_since(date).num_days() < i64::from(self.retention_days.load(Ordering::Relaxed)) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.is_dir() || is_redirect(&metadata) { continue; }
            let day = fs::canonicalize(entry.path())?;
            if day.parent() != Some(root.as_path()) { continue; }
            for file in fs::read_dir(&day)? {
                let file = file?;
                let metadata = fs::symlink_metadata(file.path())?;
                if !metadata.is_file() || is_redirect(&metadata) { continue; }
                let path = file.path();
                if !matches!(path.extension().and_then(|v| v.to_str()), Some("log" | "json" | "jsonl")) { continue; }
                let Some(stem) = path.file_stem().and_then(|v| v.to_str()) else { continue; };
                if stem.is_empty() || stem.len() > 128 || !stem.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) { continue; }
                fs::remove_file(path)?;
                removed += 1;
            }
            // An unexpected file or subdirectory keeps its date directory intact.
            if fs::read_dir(&day)?.next().is_none() { fs::remove_dir(&day)?; }
        }
        Ok(removed)
    }
    pub fn trace(self: &Arc<Self>, request: &str, session: &str, agent: &str) -> Trace {
        let id = if !request.is_empty()
            && request.len() <= 128
            && request
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            request.to_owned()
        } else {
            Sha256::digest(request.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        };
        Trace {
            sink: self.clone(),
            request: log_identifier(request),
            session: log_identifier(session),
            agent: log_identifier(agent),
            id,
        }
    }
    fn write(&self, trace: &Trace, level: Level, event: &str, fields: Value, message: Option<&str>) {
        if (level as u8) < self.level.load(Ordering::Relaxed) {
            return;
        }
        let result = (|| -> io::Result<()> {
            // Serializing and appending under one lock prevents interleaved JSON lines.
            let directory = self
                .directory
                .lock()
                .map_err(|_| io::Error::other("diagnostic writer unavailable"))?;
            let Some(directory) = directory.as_ref() else {
                return Ok(());
            };
            let now = Local::now();
            let day = directory.join(now.format("%Y-%m-%d").to_string());
            fs::create_dir_all(&day)?;
            let mut fields = clean(fields);
            // Only the explicit conversation API can persist display text. Generic
            // diagnostics still redact content/commands/credentials and cap fields.
            if let Some(message) = message { fields["message"] = Value::String(message.to_owned()); }
            let record = json!({"timestamp":now.to_rfc3339_opts(SecondsFormat::Millis,false),"level":level.text(),"traceId":trace.id,"requestId":trace.request,"sessionId":trace.session,"agentId":trace.agent,"sequence":self.sequence.fetch_add(1,Ordering::Relaxed)+1,"event":event,"fields":fields});
            let mut bytes = serde_json::to_vec(&record)?;
            bytes.push(b'\n');
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(day.join(format!("{}.log", trace.id)))?;
            file.write_all(&bytes)?;
            file.flush()
        })();
        if let Err(error) = result {
            if !self.write_failed.swap(true, Ordering::Relaxed) {
                eprintln!("ShellPilot diagnostic write failed: {:?}", error.kind());
            }
        } else {
            self.write_failed.store(false, Ordering::Relaxed);
        }
    }
}
fn is_redirect(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    { metadata.file_type().is_symlink() }
}
fn log_identifier(value: &str) -> String {
    if value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        value.into()
    } else {
        Sha256::digest(value.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
fn clean(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .take(64)
                .map(|(key, value)| {
                    let sensitive = matches!(
                        key.to_ascii_lowercase().as_str(),
                        "authorization"
                            | "apikey"
                            | "api_key"
                            | "password"
                            | "privatekey"
                            | "private_key"
                            | "secret"
                            | "headers"
                            | "prompt"
                            | "content"
                            | "command"
                            | "arguments"
                            | "stdout"
                            | "stderr"
                            | "text"
                            | "reasoning_content"
                    );
                    (
                        key,
                        if sensitive {
                            "[REDACTED]".into()
                        } else {
                            clean(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().take(64).map(clean).collect()),
        Value::String(text) if text.contains("Bearer ") || text.contains("sk-") => {
            "[REDACTED]".into()
        }
        Value::String(text) => Value::String(text.chars().take(512).collect()),
        other => other,
    }
}

#[derive(Clone)]
pub struct Trace {
    sink: Arc<Diagnostics>,
    id: String,
    request: String,
    session: String,
    agent: String,
}
tokio::task_local! { static CURRENT: Trace; }
pub trait Failure {
    fn diagnostic_code(&self) -> &str;
    fn upstream_status(&self) -> Option<u16> {
        None
    }
    fn cancelled(&self) -> bool {
        false
    }
}
impl Trace {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn event(&self, level: Level, name: &str, fields: Value) {
        self.sink.write(self, level, name, fields, None);
    }
    /// Explicit display-message logging; never pass model prompts or credential objects.
    pub fn conversation_message(&self, role: &str, connection: Option<&str>, model: &str, state: &str, message: &str) {
        self.sink.write(self, Level::Info, "conversation.message",
            json!({"role":role,"connectionName":connection,"modelId":model,"state":state}), Some(message));
    }
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        CURRENT.scope(self.clone(), future).await
    }
    pub async fn request<T, E: Failure>(
        &self,
        fields: Value,
        future: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        if CURRENT
            .try_with(|trace| trace.id == self.id && trace.session == self.session)
            .unwrap_or(false)
        {
            return future.await;
        }
        let mut guard = RequestGuard {
            trace: self.clone(),
            started: Instant::now(),
            finished: false,
        };
        self.event(Level::Info, "request.started", fields);
        let result = self.scope(future).await;
        let elapsed = guard.started.elapsed().as_millis();
        match &result {
            Ok(_) => self.event(Level::Info,"request.completed",json!({"durationMs":elapsed})),
            Err(error) => self.event(if error.cancelled() {Level::Warn} else {Level::Error},if error.cancelled() {"request.cancelled"} else {"request.failed"},json!({"durationMs":elapsed,"errorCode":error.diagnostic_code(),"httpStatus":error.upstream_status()})),
        }
        guard.finished = true;
        result
    }
}
struct RequestGuard {
    trace: Trace,
    started: Instant,
    finished: bool,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.trace.event(
                Level::Warn,
                "request.interrupted",
                json!({"durationMs":self.started.elapsed().as_millis()}),
            );
        }
    }
}
pub fn event(level: Level, name: &str, fields: Value) {
    let _ = CURRENT.try_with(|trace| trace.event(level, name, fields));
}
pub fn current() -> Option<Trace> {
    CURRENT.try_with(Clone::clone).ok()
}
pub fn inherit<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let trace = current();
    async move {
        match trace {
            Some(trace) => trace.scope(future).await,
            None => future.await,
        }
    }
}
pub fn blocking<F: FnOnce() -> T, T>(action: F) -> impl FnOnce() -> T {
    let trace = current();
    move || match trace {
        Some(trace) => CURRENT.sync_scope(trace, action),
        None => action(),
    }
}

pub fn spawn_blocking<F, T>(action: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(blocking(action))
}

#[cfg(test)]
mod tests;
