use super::{fixture, scoring::Case};
use axum::{body::Body, extract::State, response::Response, Json};
use serde::Serialize;
use serde_json::{json, Value};
use shellpilot_server::agent::provider::{self, ChatRequest, ModelClient, Usage};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    pub summary: bool,
    pub elapsed_ms: u128,
    pub estimated_input_tokens: Option<u64>,
    pub usage: Option<Usage>,
    pub error: Option<String>,
    pub finish_reason: Option<String>,
    pub summary_shape: Option<Value>,
}
#[derive(Clone)]
pub struct Bridge {
    pub client: Option<Arc<ModelClient>>,
    pub case: Case,
    pub measurements: Arc<Mutex<Vec<Measurement>>>,
    pub cancel: CancellationToken,
}
pub async fn model(State(bridge): State<Bridge>, Json(body): Json<Value>) -> Response {
    let start = Instant::now();
    let summary = body["messages"].as_array().is_some_and(|messages| {
        messages
            .iter()
            .filter(|m| m["role"] == "user")
            .filter_map(|m| m["content"].as_str())
            .filter_map(|text| serde_json::from_str::<Value>(text).ok())
            .any(|source| source["availableEvidenceEventIds"].is_array())
    });
    let request = serde_json::from_value::<ChatRequest>(json!({
        "messages":body["messages"],
        "tools":body["tools"].as_array().map(|t|t.iter().map(|t|t["function"].clone()).collect::<Vec<_>>()).unwrap_or_default()
    }));
    let measured = request
        .as_ref()
        .ok()
        .and_then(|r| {
            bridge
                .client
                .as_ref()
                .and_then(|client| provider::prepare_request(client.config(), r).ok())
        })
        .map(|p| p.estimated_input_tokens);
    let result = match (&bridge.client, request) {
        (Some(client), Ok(request)) => {
            let completion = if body["stream"] == true {
                client
                    .stream_tools(&request, &bridge.cancel, |_| async { Ok(()) })
                    .await
            } else {
                client.complete(&request, &bridge.cancel).await
            };
            completion
                .map(|c| {
                    (
                        serde_json::to_value(c.message).unwrap(),
                        c.usage,
                        c.finish_reason,
                    )
                })
                .map_err(|e| e.to_string())
        }
        (None, Ok(_)) => {
            let message = fixture::mock_reply(&bridge.case, &body);
            let reason = if message["tool_calls"]
                .as_array()
                .is_some_and(|calls| !calls.is_empty())
            {
                "tool_calls"
            } else {
                "stop"
            };
            Ok((message, None, reason.to_string()))
        }
        (_, Err(_)) => Err("E5 bridge received unsupported request shape".into()),
    };
    bridge.measurements.lock().unwrap().push(Measurement {
        summary,
        elapsed_ms: start.elapsed().as_millis(),
        estimated_input_tokens: measured,
        usage: result.as_ref().ok().and_then(|(_, usage, _)| usage.clone()),
        error: result.as_ref().err().cloned(),
        finish_reason: result.as_ref().ok().map(|(_, _, reason)| reason.clone()),
        summary_shape: summary
            .then(|| {
                result
                    .as_ref()
                    .ok()
                    .map(|(message, _, _)| summary_shape(&body, message))
            })
            .flatten(),
    });
    let (message, usage, reason) = match result {
        Ok(value) => value,
        Err(_) => return Response::builder().status(502).body(Body::empty()).unwrap(),
    };
    if body["stream"] != true {
        return Response::builder().header("content-type","application/json")
            .body(Body::from(json!({"choices":[{"index":0,"message":message,"finish_reason":reason}],"usage":usage}).to_string())).unwrap();
    }
    // Preserve the product's stream/non-stream choice upstream and collect usage;
    // emit the complete standards-shaped SSE reply to the desktop tool-turn path.
    // Therefore elapsed time measures completion latency, never first-token latency.
    let mut delta = json!({"role":"assistant"});
    if message["content"].is_string() {
        delta["content"] = message["content"].clone();
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        delta["tool_calls"] = Value::Array(
            calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut c = c.clone();
                    c["index"] = json!(i);
                    c
                })
                .collect(),
        );
    }
    let data = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}],"usage":usage})
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(data))
        .unwrap()
}

// Diagnostic shape only. Never persist source text or generated summary/fact prose.
fn summary_shape(body: &Value, message: &Value) -> Value {
    let content = message["content"].as_str().unwrap_or_default();
    let document = serde_json::from_str::<Value>(content).ok();
    let source = body["messages"].as_array().and_then(|messages| {
        messages
            .iter()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_str())
            .filter_map(|text| serde_json::from_str::<Value>(text).ok())
            .find(|value| value["availableEvidenceEventIds"].is_array())
    });
    let facts = document
        .as_ref()
        .and_then(|d| d["retainedUserFacts"].as_array());
    let invalid_fact_quotes = facts.map(|facts| {
        facts
            .iter()
            .filter(|fact| {
                let quote = fact["quote"].as_str().unwrap_or_default();
                quote.is_empty()
                    || quote.len() > 512
                    || !source
                        .as_ref()
                        .and_then(|s| s["events"].as_array())
                        .is_some_and(|events| {
                            events.iter().any(|event| {
                                event["id"] == fact["eventId"]
                                    && event["payload"]["type"] == "userMessage"
                                    && event["payload"]["text"]
                                        .as_str()
                                        .is_some_and(|text| text.contains(quote))
                            })
                        })
            })
            .count()
    });
    json!({"contentBytes":content.len(),"jsonObject":document.as_ref().is_some_and(Value::is_object),
        "summaryBytes":document.as_ref().and_then(|d|d["summary"].as_str()).map(str::len),
        "nextStepCount":document.as_ref().and_then(|d|d["nextSteps"].as_array()).map(Vec::len),
        "factCount":facts.map(Vec::len),"invalidFactQuotes":invalid_fact_quotes,
        "maxFactQuoteBytes":facts.and_then(|facts| facts.iter().filter_map(|fact|fact["quote"].as_str()).map(str::len).max()),
        "throughEventId":source.as_ref().map(|s|s["throughEventId"].clone())})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_diagnostics_count_utf8_bytes_without_exporting_prose() {
        let quote = "中".repeat(200);
        let body = json!({"messages":[{"role":"user","content":json!({
            "availableEvidenceEventIds":[1],"throughEventId":1,
            "events":[{"id":1,"payload":{"type":"userMessage","text":quote}}]
        }).to_string()}]});
        let message = json!({"content":json!({"summary":"private-summary-marker", "nextSteps":[],
            "evidenceEventIds":[1],"retainedUserFacts":[{"eventId":1,"quote":quote}]}).to_string()});
        let shape = summary_shape(&body, &message);
        assert_eq!(shape["invalidFactQuotes"], 1);
        assert_eq!(shape["maxFactQuoteBytes"], 600);
        assert!(!shape.to_string().contains("private-summary-marker"));
        assert!(!shape.to_string().contains('中'));
    }
    #[test]
    fn malformed_summary_diagnostics_never_echo_invalid_content() {
        let shape = summary_shape(&json!({}), &json!({"content":"private-invalid-json"}));
        assert_eq!(shape["jsonObject"], false);
        assert_eq!(shape["summaryBytes"], Value::Null);
        assert!(!shape.to_string().contains("private-invalid-json"));
    }
}
