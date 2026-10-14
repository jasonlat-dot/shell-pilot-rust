use super::{Error, Result, Scope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub connection_id: String,
    pub terminal_session_id: String,
}
impl Target {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.connection_id)?;
        identifier(&self.terminal_session_id)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Active,
    Completed,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub goal: String,
    pub constraints: Vec<String>,
    pub target: Option<Target>,
    pub status: TaskStatus,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub agent_id: String,
    pub user_id: String,
    pub revision: u64,
    pub task: Option<Task>,
    pub checkpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Success,
    Error,
    Unknown,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Payload {
    UserMessage {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        outcome: Outcome,
        exit_code: Option<i32>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        outputs: Vec<super::OutputReference>,
    },
    TaskStarted {
        task: Task,
    },
    Cancelled {
        reason: String,
    },
}
impl Payload {
    pub(crate) fn validate(&self) -> Result<()> {
        if serde_json::to_vec(self)?.len() > 65536 {
            return Err(Error::Invalid("event exceeds 64 KiB".into()));
        }
        match self {
            Self::UserMessage { text } | Self::AssistantMessage { text } => text_value(text, 16000),
            Self::ToolCall {
                call_id,
                tool_name,
                arguments,
            } => {
                identifier(call_id)?;
                identifier(tool_name)?;
                if !arguments.is_object() {
                    return Err(Error::Invalid("tool arguments must be an object".into()));
                }
                Ok(())
            }
            Self::ToolResult {
                call_id,
                outcome,
                exit_code,
                outputs,
                ..
            } => {
                identifier(call_id)?;
                if outputs.len() > 2 {
                    return Err(Error::Invalid("too many output references".into()));
                }
                let mut ids = std::collections::HashSet::new();
                for output in outputs {
                    output.validate()?;
                    if !ids.insert(&output.id) {
                        return Err(Error::Invalid("duplicate output reference".into()));
                    }
                }
                if matches!(outcome, Outcome::Unknown | Outcome::Cancelled) && exit_code.is_some()
                    || *outcome == Outcome::Success && exit_code.is_some_and(|code| code != 0)
                {
                    return Err(Error::Invalid("tool outcome contradicts exit code".into()));
                }
                Ok(())
            }
            Self::Cancelled { reason } => text_value(reason, 4000),
            Self::TaskStarted { .. } => Err(Error::Invalid(
                "task events are authored by the repository".into(),
            )),
        }
    }
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "user_message",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::TaskStarted { .. } => "task_started",
            Self::Cancelled { .. } => "cancelled",
        }
    }
    pub(crate) fn call_id(&self) -> Option<&str> {
        match self {
            Self::ToolCall { call_id, .. } | Self::ToolResult { call_id, .. } => Some(call_id),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: u64,
    pub session_id: String,
    pub task_id: Option<String>,
    pub request_id: String,
    pub payload: Payload,
    pub created_at: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub session: Session,
    pub through_event_id: u64,
    /// Unfinished calls remain uncertain after restart; they are never inferred successful.
    pub pending_tool_call_ids: Vec<String>,
    pub uncertain_tool_call_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointContent {
    pub task: Option<Task>,
    pub summary: String,
    pub next_steps: Vec<String>,
    pub evidence_event_ids: Vec<u64>,
    pub pending_tool_call_ids: Vec<String>,
    pub uncertain_tool_call_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub session_id: String,
    pub through_event_id: u64,
    pub source_revision: u64,
    pub committed_revision: u64,
    pub content: CheckpointContent,
    pub created_at: i64,
}
pub(crate) fn identifier(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(Error::Invalid("identifier is empty or too long".into()));
    }
    Ok(())
}
pub(crate) fn text_value(value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max {
        return Err(Error::Invalid("text is empty or too long".into()));
    }
    Ok(())
}
pub(crate) fn text_list(values: &[String]) -> Result<()> {
    if values.len() > 32 {
        return Err(Error::Invalid("too many constraints/steps".into()));
    }
    for value in values {
        text_value(value, 4000)?;
    }
    Ok(())
}

/// A replay is a receipt only, never permission to execute a side effect again.
#[derive(Clone, Debug, PartialEq)]
pub struct AppendReceipt {
    pub event: Event,
    pub inserted: bool,
}

#[derive(Clone, Debug)]
pub struct TaskRequest {
    pub goal: String,
    pub constraints: Vec<String>,
    pub target: Option<Target>,
}

/// Authored by the application from a trusted task snapshot, never by model arguments.
/// This identifies the target; it does not grant permission to execute a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolBinding {
    pub task_id: String,
    pub target: Target,
}
impl ToolBinding {
    pub fn from_snapshot(snapshot: &Snapshot) -> Result<Self> {
        let task = snapshot
            .session
            .task
            .as_ref()
            .ok_or(Error::TargetMismatch)?;
        if task.status != TaskStatus::Active {
            return Err(Error::TargetMismatch);
        }
        Ok(Self {
            task_id: task.id.clone(),
            target: task.target.clone().ok_or(Error::TargetMismatch)?,
        })
    }
    pub(crate) fn validate_call(&self, payload: &Payload) -> Result<()> {
        identifier(&self.task_id)?;
        self.target.validate()?;
        let Payload::ToolCall { arguments, .. } = payload else {
            return Err(Error::ToolProtocol);
        };
        // Routing fields, if present, must agree with the application-owned binding.
        for (name, expected) in [
            ("taskId", self.task_id.as_str()),
            ("connectionId", self.target.connection_id.as_str()),
            (
                "terminalSessionId",
                self.target.terminal_session_id.as_str(),
            ),
        ] {
            if arguments
                .get(name)
                .is_some_and(|value| value.as_str() != Some(expected))
            {
                return Err(Error::TargetMismatch);
            }
        }
        Ok(())
    }
}
