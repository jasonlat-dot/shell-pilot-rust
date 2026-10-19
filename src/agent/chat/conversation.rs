//! Display text is logged with an explicit Trace; no async scope or stream wrapper.
use super::{Request, Target};
use crate::{agent::turns::{Turn, TurnState}, diagnostics::Trace, store::Store};

pub(super) struct Conversation {
    trace: Trace,
    connection: Option<String>,
    model: String,
}
impl Conversation {
    pub(super) fn accepted(store: &Store, request: &Request, target: Option<&Target>, model: &str) -> Self {
        let connection = target.and_then(|target| store.get(&target.connection_id).ok())
            .filter(|connection| connection.user_id == request.user_id)
            .map(|connection| connection.connection_name);
        let log = Self {
            trace: store.diagnostics.trace(&request.request_id, &request.session_id, &request.agent_id),
            connection,
            model: model.to_owned(),
        };
        log.trace.conversation_message("user", log.connection.as_deref(), &log.model, "accepted", &request.message);
        log
    }
    pub(super) fn finished(&self, turn: &Turn) {
        let state = match turn.state {
            TurnState::Completed => "completed",
            TurnState::Cancelled => "cancelled",
            TurnState::Failed => "failed",
            TurnState::Interrupted => "interrupted",
            TurnState::Running => return,
        };
        let text = turn.result_text.as_deref().unwrap_or(&turn.partial_text);
        self.trace.conversation_message("assistant", self.connection.as_deref(), &self.model, state, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_message_uses_owned_connection_name_and_explicit_model_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        store.diagnostics.configure(dir.path().into(), crate::diagnostics::Level::Info).unwrap();
        store.save(crate::model::ConnectionRequest {
            connection_id: Some("server".into()), connection_name: "我的服务器".into(),
            host: "fixture".into(), port: 22, username: "fixture".into(),
            user_id: Some("default".into()), strict_host_key_check: Some(false), ..Default::default()
        }, false).unwrap();
        let request: Request = serde_json::from_value(serde_json::json!({
            "requestId":"conversation-fixture", "sessionId":"session", "agentId":"profile",
            "userId":"default", "message":"你好", "terminalSessionId":"terminal"
        })).unwrap();
        let target = Target { connection_id: "server".into(), terminal_session_id: "terminal".into() };
        let _log = Conversation::accepted(&store, &request, Some(&target), "actual-model");
        let file = dir.path().join(chrono::Local::now().format("%Y-%m-%d").to_string())
            .join("conversation-fixture.log");
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(file).unwrap().lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(rows[0]["fields"]["connectionName"], "我的服务器");
        assert_eq!(rows[0]["fields"]["modelId"], "actual-model");
        assert_eq!(rows[0]["agentId"], "profile");
        assert_eq!(rows[0]["fields"]["message"], "你好");
        assert_eq!(rows[0]["traceId"], "conversation-fixture");
    }
}
