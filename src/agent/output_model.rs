use super::{model, Error, Outcome, Result};
use serde::{Deserialize, Serialize};

pub const OUTPUT_CHUNK_BYTES: usize = 65536;
pub const OUTPUT_PREVIEW_BYTES: usize = 4096;
pub const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_OUTPUT_READ_BYTES: usize = 65536;

#[derive(Clone, Debug)]
pub struct OutputLimits {
    pub max_output_bytes: u64,
    pub max_total_bytes: u64,
    pub max_session_bytes: u64,
    pub max_outputs: usize,
    pub max_writers: usize,
    pub idle_millis: i64,
}
impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: MAX_OUTPUT_BYTES,
            max_total_bytes: 512 * 1024 * 1024,
            max_session_bytes: 128 * 1024 * 1024,
            max_outputs: 10000,
            max_writers: 4,
            idle_millis: 120000,
        }
    }
}
impl OutputLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_output_bytes == 0
            || self.max_output_bytes > MAX_OUTPUT_BYTES
            || self.max_session_bytes < self.max_output_bytes
            || self.max_total_bytes < self.max_session_bytes
            || self.max_total_bytes > 1024 * 1024 * 1024
            || !(1..=10000).contains(&self.max_outputs)
            || !(1..=64).contains(&self.max_writers)
            || !(1..=3600000).contains(&self.idle_millis)
        {
            return Err(Error::Invalid("invalid output storage limits".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputChannel {
    Stdout,
    Stderr,
}
impl OutputChannel {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            _ => Err(Error::OutputUnavailable),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputReference {
    pub id: String,
    pub channel: OutputChannel,
    pub size: u64,
    pub sha256: String,
}
impl OutputReference {
    pub(crate) fn validate(&self) -> Result<()> {
        if !uuid::Uuid::parse_str(&self.id).is_ok_and(|id| id.to_string() == self.id)
            || self.size > MAX_OUTPUT_BYTES
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid("invalid output reference".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredOutput {
    pub reference: OutputReference,
    pub session_id: String,
    pub task_id: String,
    pub call_id: String,
    pub preview: String,
    pub preview_truncated: bool,
    pub preview_lossy: bool,
    pub event_id: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputRange {
    /// Original bytes. Offsets may bisect UTF-8; concatenate/decode with a streaming decoder.
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub next_offset: u64,
    pub eof: bool,
    pub size: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputStats {
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub outputs: usize,
    pub active_writers: usize,
}
pub struct CaptureRequest {
    pub request_id: String,
    pub call_id: String,
    pub channel: OutputChannel,
}
impl CaptureRequest {
    pub(crate) fn validate(&self) -> Result<()> {
        model::identifier(&self.request_id)?;
        model::identifier(&self.call_id)
    }
}
pub struct CapturedResult {
    pub call_id: String,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub output_ids: Vec<String>,
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
